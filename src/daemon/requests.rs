//! Common admission and durable lifecycle for every business operation.
use super::{OperationJob, Runtime, files, jobs, streams};
use crate::{error::ErrorCode, net::Ws, protocol::*};
use anyhow::{Context, Result, bail};
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;

pub(super) async fn serve(
    rt: Arc<Runtime>,
    source: &str,
    generation: u64,
    ws: &mut Ws,
    request: Request,
) -> Result<()> {
    rt.check_session(source, generation)?;
    if matches!(
        request,
        Request::Exec { .. }
            | Request::Jobs { .. }
            | Request::Kill { .. }
            | Request::Wait { .. }
            | Request::Logs { .. }
    ) {
        return jobs::serve(rt, source, generation, ws, request).await;
    }
    let runtime = rt.clone();
    let owner = source.to_owned();
    let intent = request.clone();
    let Admission {
        job,
        fresh,
        permit,
        mut operation,
    } = tokio::task::spawn_blocking(move || admit(&runtime, &owner, generation, &intent)).await??;
    if !fresh {
        crate::net::send(ws, &Data::Accepted { job, fresh: false }).await?;
        return Ok(());
    }
    crate::net::send(
        ws,
        &Data::Accepted {
            job: job.clone(),
            fresh: true,
        },
    )
    .await?;
    if job.kind() != JobKind::StreamExec {
        rt.store.mark_running(&job.job_id).await?;
    }
    let kind = job.kind();
    let runtime = rt.clone();
    let id = job.job_id.clone();
    let result = {
        let handler = async {
            match request {
                request @ (Request::Push { .. }
                | Request::Pull { .. }
                | Request::Screenshot { .. }) => {
                    files::serve(
                        rt,
                        source,
                        generation,
                        ws,
                        request,
                        &mut operation,
                        permit.context("missing file admission")?,
                    )
                    .await
                }
                request @ (Request::StreamExec { .. } | Request::Forward { .. }) => {
                    streams::serve(rt, source, generation, ws, request, &mut operation, permit)
                        .await
                }
                _ => unreachable!("queries were dispatched before admission"),
            }
        };
        if kind == JobKind::StreamExec {
            Some(handler.await)
        } else {
            tokio::select! {
                result = handler => Some(result),
                _ = wait_canceled(&runtime, &id) => None,
            }
        }
    };
    let result = match result {
        Some(result) => result,
        None => {
            if let Some(operation) = operation.as_mut() {
                operation.cancel()?;
            }
            Err(ErrorCode::JobCanceled
                .error("operation canceled by its source device")
                .into())
        }
    };
    if let Err(error) = &result
        && let Some(operation) = operation.as_mut()
    {
        operation.fail(error)?;
    }
    if runtime
        .store
        .get_async(&id)
        .await
        .ok()
        .flatten()
        .is_some_and(|job| job.state.terminal())
    {
        runtime.canceled.lock().unwrap().remove(&id);
    }
    result
}
struct Admission {
    job: Job,
    fresh: bool,
    permit: Option<OwnedSemaphorePermit>,
    operation: Option<OperationJob>,
}
fn admit(rt: &Runtime, source: &str, generation: u64, request: &Request) -> Result<Admission> {
    let _gate = rt.gate.lock().unwrap();
    rt.check_session(source, generation)?;
    if rt.stopping.load(std::sync::atomic::Ordering::SeqCst) {
        bail!(ErrorCode::DaemonStopping.error("daemon shutting down"));
    }
    let context = request.job_context().context("missing job identity")?;
    if context.db_id != rt.store.db_id {
        bail!(ErrorCode::DbReset.error("original database no longer exists"));
    }
    if context.request_id.is_empty() || context.request_id.len() > 128 {
        bail!(ErrorCode::InvalidRequest.error("invalid request-id"));
    }
    let hash = request.job_hash().context("not a business operation")?;
    if let Some(job) = rt.store.by_request(source, &context.request_id)? {
        if job.request_hash != hash {
            bail!(
                ErrorCode::RequestConflict
                    .error("request-id reused with different operation parameters")
            );
        }
        return Ok(Admission {
            job,
            fresh: false,
            permit: None,
            operation: None,
        });
    }
    match request {
        Request::Push {
            sha256,
            no_overwrite,
            expect,
            ..
        } => {
            if sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!(ErrorCode::InvalidRequest.error("invalid file digest"));
            }
            if *no_overwrite && expect.is_some() {
                bail!(ErrorCode::InvalidRequest.error("expect conflicts with no-overwrite"));
            }
        }
        Request::StreamExec { execution, .. } => super::program::validate_stream(execution)?,
        Request::Forward { port: 0, .. } => {
            bail!(ErrorCode::InvalidPort.error("remote port must be 1..65535"))
        }
        _ => {}
    }
    let permit = match request {
        Request::Push { .. } | Request::Pull { .. } | Request::Screenshot { .. } => Some(
            rt.files
                .clone()
                .try_acquire_owned()
                .context(ErrorCode::DeviceBusy.error("too many file operations"))?,
        ),
        Request::Forward { .. } => Some(
            rt.forwards
                .clone()
                .try_acquire_owned()
                .context(ErrorCode::DeviceBusy.error("too many forwarded connections"))?,
        ),
        Request::StreamExec { .. } => {
            super::execution::check_capacity(rt)?;
            None
        }
        _ => unreachable!(),
    };
    let mut job = Job::accepted(
        source,
        &rt.id.device_id,
        &context,
        hash,
        request.job_details().context("missing parameters")?,
    );
    while rt.store.get(&job.job_id)?.is_some() {
        job.job_id = new_job_id();
    }
    rt.store.insert(&job)?;
    let operation = Some(OperationJob {
        store: rt.store.clone(),
        job_id: job.job_id.clone(),
        finished: false,
        attachment_error: None,
        leftover_possible: false,
    });
    Ok(Admission {
        job,
        fresh: true,
        permit,
        operation,
    })
}

pub(super) async fn wait_canceled(rt: &Runtime, id: &str) {
    loop {
        if rt.canceled.lock().unwrap().contains(id) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
