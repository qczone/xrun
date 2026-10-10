//! File and screenshot handlers update the job accepted by the common dispatcher.
use super::{OperationJob, Runtime};
use crate::{
    error::ErrorCode,
    net::{self, Ws},
    protocol::*,
};
use anyhow::{Result, bail};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::OwnedSemaphorePermit;

pub(super) async fn serve(
    rt: Arc<Runtime>,
    source: &str,
    generation: u64,
    ws: &mut Ws,
    request: Request,
    operation: &mut Option<OperationJob>,
    permit: OwnedSemaphorePermit,
) -> Result<()> {
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
            let contents = net::receive_file(ws, size, &sha256).await?;
            rt.check_session(source, generation)?;
            let path = crate::transfer::remote_path(&path, &operation_cwd(&rt, cwd)?)?;
            if let Some(job) = operation.as_mut() {
                job.retain_file(contents.path(), path.to_string_lossy().into_owned())
                    .await;
            }
            let mut owned_job = operation.take();
            let hash = sha256.clone();
            let runtime = rt.clone();
            let path = crate::transfer::push(
                path,
                contents,
                mkdir,
                no_overwrite,
                expect,
                move |path, published, error| {
                    let _permit = permit;
                    if let Some(mut job) = owned_job.take() {
                        let canceled = runtime.canceled.lock().unwrap().remove(&job.job_id);
                        if published {
                            job.finish(
                                JobState::Succeeded,
                                JobResult::File(FileResult {
                                    path: path.to_string_lossy().into(),
                                    size,
                                    sha256: hash,
                                    attachment_error: None,
                                }),
                            )?;
                        } else if canceled {
                            job.cancel()?;
                        } else if let Some(error) = error {
                            job.fail(error)?;
                        }
                    }
                    Ok(())
                },
            )
            .await?;
            net::send(
                ws,
                &Data::File {
                    path: path.to_string_lossy().into(),
                    size,
                    sha256,
                    width: None,
                    height: None,
                    captured_at: None,
                },
            )
            .await?;
        }
        Request::Pull { path, cwd, .. } => {
            let path = crate::transfer::remote_path(&path, &operation_cwd(&rt, cwd)?)?;
            let (contents, size, hash, _permit) =
                crate::transfer::snapshot(path.clone(), permit).await?;
            net::send(
                ws,
                &Data::File {
                    path: path.to_string_lossy().into(),
                    size,
                    sha256: hash.clone(),
                    width: None,
                    height: None,
                    captured_at: None,
                },
            )
            .await?;
            net::send_file(ws, contents.as_file()).await?;
            if let Some(job) = operation.as_mut() {
                job.retain_file(contents.path(), path.to_string_lossy().into_owned())
                    .await;
                job.finish(
                    JobState::Succeeded,
                    JobResult::File(FileResult {
                        path: path.to_string_lossy().into(),
                        size,
                        sha256: hash,
                        attachment_error: None,
                    }),
                )?;
            }
        }
        Request::Screenshot { .. } => {
            let (capture, _permit) = crate::screenshot::capture_with_permit(permit).await?;
            send_capture(ws, capture, operation).await?;
        }
        _ => bail!(ErrorCode::InvalidRequest.error("operation dispatched to the wrong handler")),
    }
    Ok(())
}
async fn send_capture(
    ws: &mut Ws,
    capture: crate::screenshot::Capture,
    operation: &mut Option<OperationJob>,
) -> Result<()> {
    let result = ScreenshotResult {
        captured_at: capture.at.clone(),
        width: capture.width,
        height: capture.height,
        size: capture.bytes.len() as u64,
        sha256: sha256(&capture.bytes),
        attachment_error: None,
    };
    net::send(
        ws,
        &Data::File {
            path: String::new(),
            size: result.size,
            sha256: result.sha256.clone(),
            width: Some(result.width),
            height: Some(result.height),
            captured_at: Some(result.captured_at.clone()),
        },
    )
    .await?;
    net::send_bytes(ws, &capture.bytes).await?;
    if let Some(job) = operation.as_mut() {
        job.retain_bytes(capture.bytes, format!("screenshot-{}.png", now_ms()))
            .await;
        job.finish(JobState::Succeeded, JobResult::Screenshot(result))?;
    }
    Ok(())
}
fn operation_cwd(rt: &Runtime, cwd: Option<String>) -> Result<PathBuf> {
    let path = cwd.map(PathBuf::from).unwrap_or(rt.cwd()?);
    if !path.is_absolute() {
        bail!(ErrorCode::InvalidCwd.error("-C must be absolute"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::protocol::Role;

    async fn screenshot_job(store: &Arc<crate::store::JobStore>) -> Result<OperationJob> {
        let job = Job::accepted(
            "source",
            "target",
            &JobContext::new(&store.db_id),
            "hash".into(),
            JobDetails::Screenshot(ScreenshotParams {}),
        );
        store.insert(&job)?;
        store.mark_running(&job.job_id).await?;
        Ok(OperationJob {
            store: store.clone(),
            job_id: job.job_id,
            finished: false,
            attachment_error: None,
            leftover_possible: false,
        })
    }

    #[tokio::test]
    async fn screenshot_wire_metadata_and_job_agree_and_disconnects_are_not_successes() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let store = Arc::new(crate::store::JobStore::open(
            &temp.path().join("daemon.db"),
            true,
        )?);
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, 1, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&[255, 0, 0])?;
        }
        for disconnected in [false, true] {
            let (a, b) = tokio::io::duplex(4096);
            let mut server =
                Ws::from_raw_socket(net::SocketIo::new(Box::new(a)), Role::Server, None).await;
            let mut client =
                Ws::from_raw_socket(net::SocketIo::new(Box::new(b)), Role::Client, None).await;
            let mut operation = Some(screenshot_job(&store).await?);
            let id = operation.as_ref().unwrap().job_id.clone();
            let capture = crate::screenshot::Capture {
                bytes: png.clone(),
                width: 1,
                height: 1,
                at: "2026-10-06T00:00:00Z".into(),
            };
            if disconnected {
                drop(client);
                assert!(
                    send_capture(&mut server, capture, &mut operation)
                        .await
                        .is_err()
                );
                assert!(!operation.as_ref().unwrap().finished);
            } else {
                send_capture(&mut server, capture, &mut operation).await?;
                let Data::File {
                    path,
                    size,
                    sha256: hash,
                    width,
                    height,
                    captured_at,
                } = net::receive(&mut client).await?
                else {
                    bail!("file metadata expected")
                };
                assert!(path.is_empty());
                assert_eq!((width, height), (Some(1), Some(1)));
                assert_eq!(captured_at.as_deref(), Some("2026-10-06T00:00:00Z"));
                assert_eq!(
                    net::receive_bytes(&mut client, size, &hash, MAX_SCREENSHOT).await?,
                    png
                );
                assert!(operation.as_ref().unwrap().finished);
                let job = store.get(&id)?.unwrap();
                let JobResult::Screenshot(result) = job.result.unwrap() else {
                    panic!("screenshot result")
                };
                assert_eq!(result.size, png.len() as u64);
                assert_eq!(result.sha256, sha256(&png));
                assert_eq!(result.captured_at, "2026-10-06T00:00:00Z");
                assert_eq!(job.attachments.len(), 1);
                assert!(
                    store
                        .attachments()
                        .preview(job.attachments[0].metadata.clone())?
                        .image
                        .is_some()
                );
            }
            drop(operation);
            store.flush().await?;
            assert_eq!(
                store.get(&id)?.unwrap().state,
                if disconnected {
                    JobState::Lost
                } else {
                    JobState::Succeeded
                }
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn attachment_failure_keeps_successful_screenshot_outcome() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = Arc::new(crate::store::JobStore::open(
            &temp.path().join("daemon.db"),
            true,
        )?);
        std::fs::write(temp.path().join("attachments"), b"cache unavailable")?;
        let (a, b) = tokio::io::duplex(4096);
        let mut server =
            Ws::from_raw_socket(net::SocketIo::new(Box::new(a)), Role::Server, None).await;
        let mut client =
            Ws::from_raw_socket(net::SocketIo::new(Box::new(b)), Role::Client, None).await;
        let bytes = b"fixture image bytes".to_vec();
        let mut operation = Some(screenshot_job(&store).await?);
        let id = operation.as_ref().unwrap().job_id.clone();
        send_capture(
            &mut server,
            crate::screenshot::Capture {
                bytes: bytes.clone(),
                width: 1,
                height: 1,
                at: "fixture".into(),
            },
            &mut operation,
        )
        .await?;
        let Data::File {
            size, sha256: hash, ..
        } = net::receive(&mut client).await?
        else {
            bail!("file metadata expected")
        };
        assert_eq!(
            net::receive_bytes(&mut client, size, &hash, MAX_SCREENSHOT).await?,
            bytes
        );
        let job = store.get(&id)?.unwrap();
        assert_eq!(job.state, JobState::Succeeded);
        assert!(job.attachments.is_empty());
        let JobResult::Screenshot(result) = job.result.unwrap() else {
            panic!("screenshot result")
        };
        assert!(result.attachment_error.is_some());
        Ok(())
    }
}
