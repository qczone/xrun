//! Session-bound commands and forwarding share the durable job lifecycle.
use super::{
    OperationJob, RunningStream, Runtime,
    process_identity::{boot_id, process_start},
    program::resolve_program,
};
use crate::{
    error::ErrorCode,
    net::{self, Ws},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
};
use tokio::sync::OwnedSemaphorePermit;

pub(super) async fn serve(
    rt: Arc<Runtime>,
    source: &str,
    generation: u64,
    ws: &mut Ws,
    request: Request,
    operation: &mut Option<OperationJob>,
    permit: Option<OwnedSemaphorePermit>,
) -> Result<()> {
    let id = operation
        .as_ref()
        .context("missing accepted job")?
        .job_id
        .clone();
    match request {
        Request::StreamExec { execution, .. } => {
            let counts = Arc::new(crate::streaming::Counts::default());
            let cfg = rt.config()?;
            let cwd = PathBuf::from(&execution.cwd);
            let mut env = cfg.env;
            for (key, value) in &execution.env {
                #[cfg(windows)]
                env.retain(|name, _| !name.eq_ignore_ascii_case(key));
                env.insert(key.clone(), value.clone());
            }
            let program = resolve_program(&execution.program, &cwd, &env)?;
            let runtime = rt.clone();
            let owner = source.to_owned();
            let process_id = id.clone();
            let args = execution.args.clone();
            let child = tokio::task::spawn_blocking(move || {
                let _gate = runtime.gate.lock().unwrap();
                runtime.check_session(&owner, generation)?;
                if runtime.stopping.load(Ordering::SeqCst) {
                    bail!(ErrorCode::DaemonStopping.error("daemon shutting down"));
                }
                if runtime.canceled.lock().unwrap().contains(&process_id) {
                    runtime.store.finish_sync(
                        &process_id,
                        crate::store::JobOutcome::new(JobState::Canceled),
                    )?;
                    bail!(ErrorCode::JobCanceled.error("operation canceled before process launch"));
                }
                let child = crate::process::spawn(&program, &args, &cwd, &env, &process_id, false)?;
                if let Err(error) = runtime.store.record_process(
                    &process_id,
                    ProcessIdentity {
                        pid: child.pid,
                        boot_id: boot_id(),
                        start: process_start(child.pid),
                    },
                ) {
                    crate::process::force_kill(child.pid);
                    return Err(error);
                }
                runtime
                    .running
                    .lock()
                    .unwrap()
                    .insert(process_id, child.pid);
                runtime.streams.fetch_add(1, Ordering::SeqCst);
                Ok::<_, anyhow::Error>(child)
            })
            .await??;
            if let Some(job) = operation.as_mut() {
                job.leftover_possible = cfg!(unix);
            }
            let running = RunningStream {
                rt: rt.clone(),
                id: id.clone(),
            };
            rt.store.mark_running(&id).await?;
            let mut job = operation.take().context("missing streaming job")?;
            crate::streaming::serve(
                ws,
                child,
                execution.timeout,
                counts,
                super::requests::wait_canceled(&rt, &id),
                move |result, counts| {
                    drop(running);
                    let state = if result.canceled {
                        JobState::Canceled
                    } else if result.timed_out {
                        JobState::TimedOut
                    } else if result.exit_code == Some(0) && result.signal.is_none() {
                        JobState::Succeeded
                    } else {
                        JobState::Failed
                    };
                    job.finish(
                        state,
                        JobResult::Command(CommandResult {
                            exit_code: result.exit_code,
                            signal: result.signal,
                            duration_ms: result.duration_ms,
                            input_bytes: Some(counts.input.load(Ordering::Relaxed)),
                            stdout_bytes: Some(counts.stdout.load(Ordering::Relaxed)),
                            stderr_bytes: Some(counts.stderr.load(Ordering::Relaxed)),
                        }),
                    )
                },
            )
            .await?;
        }
        Request::Forward { port, .. } => {
            let _permit = permit.context("missing forwarding admission")?;
            let started = std::time::Instant::now();
            let tcp = crate::forwarding::connect_loopback(port).await?;
            rt.check_session(source, generation)?;
            net::send(ws, &Data::ForwardReady { port }).await?;
            let (input_bytes, output_bytes) = crate::forwarding::bridge(ws, tcp).await?;
            if let Some(job) = operation.as_mut() {
                job.finish(
                    JobState::Succeeded,
                    JobResult::Forward(ForwardResult {
                        port,
                        duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
                        input_bytes,
                        output_bytes,
                    }),
                )?;
            }
        }
        _ => bail!(ErrorCode::InvalidRequest.error("operation dispatched to the wrong handler")),
    }
    Ok(())
}
