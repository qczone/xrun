//! Job-owned immutable snapshots, hash skips and explicit recovery of interrupted transfers.
use super::{OperationJob, Runtime};
use crate::{
    config,
    error::ErrorCode,
    net::{self, Ws},
    protocol::*,
    transfer::{
        self,
        checkpoint::{Checkpoint, RETENTION_MS},
    },
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::OwnedSemaphorePermit;
#[derive(Clone, Serialize, Deserialize)]
struct State {
    network: String,
    db_id: String,
    source: String,
    target: String,
    job_id: String,
    request_id: String,
    request_hash: String,
    path: PathBuf,
    size: u64,
    sha256: String,
    offset: u64,
    expires_at_ms: i64,
}
fn dir() -> Result<PathBuf> {
    Ok(config::device_dir()?.join("job-transfers"))
}
fn bound(state: &State, job: &Job, network: &str) -> bool {
    state.network == network
        && state.db_id == job.db_id
        && state.source == job.source_device_id
        && state.target == job.target_device_id
        && state.job_id == job.job_id
        && state.request_id == job.request_id
        && state.request_hash == job.request_hash
        && state.offset <= state.size
        && transfer::valid_hash(&state.sha256)
        && now_ms() < state.expires_at_ms
}
pub(super) fn recover_at(base: &Path, job: &Job) -> Result<bool> {
    if !matches!(job.kind(), JobKind::Push | JobKind::Pull) {
        return Ok(false);
    }
    let cp = Checkpoint::<State>::open(&base.join("job-transfers"), &job.job_id)?;
    let id: config::Identity = config::read(&config::network_dir_at(base)?.join("identity.toml"))?;
    Ok(cp.state.as_ref().is_some_and(|state| {
        bound(
            state,
            job,
            &id.network
                .as_ref()
                .map_or(String::new(), |n| n.network_id.clone()),
        ) && std::fs::metadata(&cp.blob).is_ok_and(|m| m.is_file() && m.len() >= state.offset)
    }))
}
pub(super) fn expire_at(base: &Path, store: &crate::store::JobStore) -> Result<()> {
    transfer::checkpoint::expire(&base.join("job-transfers"), |id| {
        if store.get(id)?.is_some_and(|job| !job.state.terminal()) {
            store.finish_sync(
                id,
                crate::store::JobOutcome::failed(
                    &ErrorCode::Unconfirmed
                        .error("transfer staging expired; create a new intent")
                        .into(),
                ),
            )?;
        }
        Ok(())
    })
}
pub(super) fn disconnected(error: &anyhow::Error) -> bool {
    crate::error::is(error, ErrorCode::ConnectionClosed)
        || error
            .downcast_ref::<tokio_tungstenite::tungstenite::Error>()
            .is_some()
        || error.downcast_ref::<std::io::Error>().is_some_and(|e| {
            matches!(
                e.kind(),
                std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::UnexpectedEof
            )
        })
}
pub(super) async fn serve(
    rt: Arc<Runtime>,
    source: &str,
    generation: u64,
    ws: &mut Ws,
    request: Request,
    operation: &mut Option<OperationJob>,
    permit: OwnedSemaphorePermit,
) -> Result<()> {
    let job = rt
        .store
        .get(operation.as_ref().context("file job")?.job_id.as_str())?
        .context("file job missing")?;
    let mut cp = Checkpoint::<State>::open(&dir()?, &job.job_id)?;
    if cp.state.is_none() {
        // Pending staging slots are separate from live connection admission and expire.
        let mut pending = 0;
        for entry in std::fs::read_dir(dir()?)? {
            let entry = entry?;
            if entry.path().extension().is_some_and(|ext| ext == "json") {
                let key = entry
                    .path()
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if rt.store.get(&key)?.is_none_or(|job| !job.state.terminal()) {
                    pending += 1;
                }
            }
        }
        if pending >= 32 {
            bail!(
                ErrorCode::DeviceBusy
                    .error("too many saved transfers; resume or cancel an existing transfer")
            );
        }
    }
    match request {
        Request::Push {
            path,
            cwd,
            size,
            sha256,
            mkdir,
            no_overwrite,
            expect,
            ..
        } => {
            let path = if let Some(state) = &cp.state {
                state.path.clone()
            } else {
                transfer::remote_path(&path, &super::files::operation_cwd(&rt, cwd)?)?
            };
            let prepared = transfer::prepare_destination(&path, mkdir).await?;
            let same = transfer::matches_destination(
                &prepared,
                size,
                &sha256,
                no_overwrite,
                expect.as_deref(),
            )?;
            if same {
                net::send(
                    ws,
                    &Data::TransferOffer {
                        size,
                        sha256: sha256.clone(),
                        offset: size,
                        skipped: true,
                    },
                )
                .await?;
                let job = operation.as_mut().unwrap();
                job.retain_file(&prepared.path, prepared.path.to_string_lossy().into())
                    .await;
                job.finish(
                    JobState::Succeeded,
                    JobResult::File(FileResult {
                        path: prepared.path.to_string_lossy().into(),
                        size,
                        sha256: sha256.clone(),
                        attachment_error: None,
                    }),
                )?;
                send_header(ws, &prepared.path, size, &sha256).await
            } else {
                if cp.state.is_none() {
                    let file = cp.new_blob()?;
                    file.sync_all()?;
                    cp.save(state(
                        &rt,
                        &job,
                        prepared.path.clone(),
                        size,
                        sha256.clone(),
                    ))?;
                }
                let mut saved = cp.state.clone().context("transfer checkpoint missing")?;
                if !bound(&saved, &job, &rt.network_id)
                    || saved.size != size
                    || saved.sha256 != sha256
                {
                    bail!(ErrorCode::RequestConflict.error("saved upload differs from this job"));
                }
                let file = std::fs::File::options()
                    .read(true)
                    .write(true)
                    .open(&cp.blob)?;
                net::send(
                    ws,
                    &Data::TransferOffer {
                        size,
                        sha256: sha256.clone(),
                        offset: saved.offset,
                        skipped: false,
                    },
                )
                .await?;
                let received =
                    net::receive_remaining(ws, &file, size, &sha256, saved.offset, |offset| {
                        saved.offset = offset;
                        cp.save(saved.clone())
                    })
                    .await;
                if let Err(error) = received {
                    if disconnected(&error) {
                        operation.as_mut().unwrap().suspend()?;
                    }
                    return Err(error);
                }
                rt.check_session(source, generation)?;
                operation
                    .as_mut()
                    .unwrap()
                    .retain_file(&cp.blob, prepared.path.to_string_lossy().into())
                    .await;
                let contents = tempfile::NamedTempFile::from_parts(
                    file,
                    tempfile::TempPath::try_from_path(cp.blob.clone())?,
                );
                let mut job = operation.take().unwrap();
                let digest = sha256.clone();
                let path = transfer::push_prepared(
                    prepared,
                    contents,
                    no_overwrite,
                    expect,
                    move |path, published, error| {
                        let _permit = permit;
                        let mut checkpoint = cp;
                        if published {
                            job.finish(
                                JobState::Succeeded,
                                JobResult::File(FileResult {
                                    path: path.to_string_lossy().into(),
                                    size,
                                    sha256: digest,
                                    attachment_error: None,
                                }),
                            )?;
                            if let Err(error) = checkpoint.clear() {
                                tracing::warn!(%error,"completed upload staging cleanup failed");
                            }
                            Ok(())
                        } else if let Some(error) = error {
                            job.fail(error)
                        } else {
                            job.cancel()
                        }
                    },
                )
                .await?;
                send_header(ws, &path, size, &sha256).await
            }
        }
        Request::Pull { path, cwd, .. } => {
            let path = if let Some(state) = &cp.state {
                state.path.clone()
            } else {
                transfer::remote_path(&path, &super::files::operation_cwd(&rt, cwd)?)?
            };
            let (size, hash, _permit) = if let Some(state) = &cp.state {
                (state.size, state.sha256.clone(), permit)
            } else {
                let (snapshot, size, hash, permit) =
                    transfer::snapshot(path.clone(), permit).await?;
                snapshot.as_file().sync_all()?;
                cp.adopt(snapshot)?;
                cp.save(state(&rt, &job, path.clone(), size, hash.clone()))?;
                (size, hash, permit)
            };
            let mut saved = cp.state.clone().unwrap();
            if !bound(&saved, &job, &rt.network_id) {
                bail!(ErrorCode::RequestConflict.error("saved download differs from this job"));
            }
            let file = std::fs::File::open(&cp.blob)?;
            send_header(ws, &path, size, &hash).await?;
            let Data::TransferWant {
                offset,
                sha256: prefix,
            } = net::receive(ws).await?
            else {
                bail!(ErrorCode::InvalidMessage.error("expected download cursor"));
            };
            if offset > size || net::prefix_hash(&file, offset)? != prefix {
                bail!(
                    ErrorCode::ChecksumMismatch
                        .error("download prefix differs from immutable snapshot")
                );
            }
            net::send(
                ws,
                &Data::TransferOffer {
                    size,
                    sha256: hash.clone(),
                    offset,
                    skipped: offset == size,
                },
            )
            .await?;
            let sent = if offset != size {
                net::send_remaining(ws, &file, size, offset, |offset| {
                    saved.offset = offset;
                    cp.save(saved.clone())
                })
                .await
            } else {
                Ok(())
            };
            if let Err(error) = sent {
                if disconnected(&error) {
                    operation.as_mut().unwrap().suspend()?;
                }
                return Err(error);
            }
            let operation = operation.as_mut().unwrap();
            if !operation.finished {
                operation
                    .retain_file(&cp.blob, path.to_string_lossy().into())
                    .await;
                operation.finish(
                    JobState::Succeeded,
                    JobResult::File(FileResult {
                        path: path.to_string_lossy().into(),
                        size,
                        sha256: hash,
                        attachment_error: None,
                    }),
                )?;
                if let Err(error) = cp.clear() {
                    tracing::warn!(%error,"completed download staging cleanup failed");
                }
            }
            Ok(())
        }
        _ => bail!(ErrorCode::InvalidRequest.error("expected a file intent")),
    }
}
fn state(rt: &Runtime, job: &Job, path: PathBuf, size: u64, sha256: String) -> State {
    State {
        network: rt.network_id.clone(),
        db_id: job.db_id.clone(),
        source: job.source_device_id.clone(),
        target: job.target_device_id.clone(),
        job_id: job.job_id.clone(),
        request_id: job.request_id.clone(),
        request_hash: job.request_hash.clone(),
        path,
        size,
        sha256,
        offset: 0,
        expires_at_ms: now_ms() + RETENTION_MS,
    }
}
async fn send_header(ws: &mut Ws, path: &Path, size: u64, hash: &str) -> Result<()> {
    net::send(
        ws,
        &Data::File {
            path: path.to_string_lossy().into(),
            size,
            sha256: hash.into(),
            width: None,
            height: None,
            captured_at: None,
            format: None,
            mime: None,
        },
    )
    .await
}
pub(super) async fn resume(
    rt: Arc<Runtime>,
    source: &str,
    generation: u64,
    ws: &mut Ws,
    id: &str,
    context: JobContext,
) -> Result<()> {
    let job = rt.owned_job(source, id).await?;
    if job.db_id != context.db_id || job.request_id != context.request_id {
        bail!(ErrorCode::DbReset.error("transfer binding differs from its original job"));
    }
    if !matches!(job.kind(), JobKind::Push | JobKind::Pull) {
        bail!(ErrorCode::InvalidRequest.error("only file transfers can be resumed"));
    }
    if job.state.terminal() && job.state != JobState::Succeeded {
        bail!(ErrorCode::InvalidRequest.error("canceled or failed transfer cannot be resumed"));
    }
    // Probe the lock before constructing a lifecycle owner; concurrent recovery
    // must never change the state of the transfer already running.
    let probe = Checkpoint::<State>::open(&dir()?, id)?;
    if job.state == JobState::Succeeded {
        let Some(JobResult::File(result)) = &job.result else {
            bail!(ErrorCode::InvalidMessage.error("missing prior upload result"));
        };
        net::send(
            ws,
            &Data::Accepted {
                job: job.clone(),
                fresh: false,
            },
        )
        .await?;
        if job.kind() == JobKind::Pull {
            send_header(ws, Path::new(&result.path), result.size, &result.sha256).await?;
            let Data::TransferWant { offset, sha256 } = net::receive(ws).await? else {
                bail!(ErrorCode::InvalidMessage.error("expected original download cursor"));
            };
            if offset != result.size || sha256 != result.sha256 {
                bail!(ErrorCode::Unconfirmed.error(
                    "completed download must be recovered from its fully acknowledged local copy"
                ));
            }
            return net::send(
                ws,
                &Data::TransferOffer {
                    size: result.size,
                    sha256: result.sha256.clone(),
                    offset: result.size,
                    skipped: true,
                },
            )
            .await;
        }
        net::send(
            ws,
            &Data::TransferOffer {
                size: result.size,
                sha256: result.sha256.clone(),
                offset: result.size,
                skipped: true,
            },
        )
        .await?;
        return send_header(ws, Path::new(&result.path), result.size, &result.sha256).await;
    }
    if probe
        .state
        .as_ref()
        .is_none_or(|state| !bound(state, &job, &rt.network_id))
    {
        bail!(
            ErrorCode::Unconfirmed
                .error("transfer staging unavailable or expired; query the original result")
        );
    }
    let permit = rt
        .files
        .clone()
        .try_acquire_owned()
        .context(ErrorCode::DeviceBusy.error("too many file operations"))?;
    let request = match &job.details {
        JobDetails::Push(p) => Request::Push {
            context,
            path: p.path.clone(),
            cwd: p.cwd.clone(),
            size: p.size,
            sha256: p.sha256.clone(),
            mkdir: p.mkdir,
            no_overwrite: p.no_overwrite,
            expect: p.expect.clone(),
        },
        JobDetails::Pull(p) => Request::Pull {
            context,
            path: p.path.clone(),
            cwd: p.cwd.clone(),
        },
        _ => unreachable!(),
    };
    drop(probe);
    if !job.state.terminal() {
        rt.store.mark_running(id).await?;
    }
    net::send(
        ws,
        &Data::Accepted {
            job: job.clone(),
            fresh: false,
        },
    )
    .await?;
    let mut operation = Some(OperationJob {
        store: rt.store.clone(),
        job_id: id.into(),
        finished: job.state.terminal(),
        attachment_error: None,
        leftover_possible: false,
    });
    let result = serve(rt, source, generation, ws, request, &mut operation, permit).await;
    if let Err(error) = &result
        && let Some(operation) = operation.as_mut()
    {
        operation.fail(error)?;
    }
    result
}

pub(super) fn cancel_pending(rt: &Runtime, id: &str) -> Result<bool> {
    let job = rt.store.get(id)?.context("file job missing")?;
    if job.state != JobState::Interrupted {
        return Ok(false);
    }
    let Ok(mut cp) = Checkpoint::<State>::open(&dir()?, id) else {
        return Ok(false);
    };
    rt.store.finish_sync(
        id,
        crate::store::JobOutcome {
            error_code: Some("JOB_CANCELED".into()),
            error_message: Some("transfer canceled by its source".into()),
            ..crate::store::JobOutcome::new(JobState::Canceled)
        },
    )?;
    cp.clear()?;
    Ok(true)
}
