//! Upload, download and screenshot commands.
use crate::error::ErrorCode;
use crate::{
    config::{self, Identity},
    net,
    protocol::*,
};
use anyhow::{Context, Result, bail};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

use super::args::Remote;
use super::support::*;

fn save_download(
    bytes: &[u8],
    path: Option<PathBuf>,
    prefix: &str,
    suffix: &str,
) -> Result<PathBuf> {
    if let Some(path) = path {
        return crate::transfer::save_local(&std::env::current_dir()?.join(path), bytes);
    }
    let mut temp = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .tempfile()?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    config::sync_parent(temp.path())?;
    let (_, path) = temp.keep()?;
    Ok(path)
}

fn interrupted_upload(sent: &std::sync::atomic::AtomicBool, json: bool) -> i32 {
    if sent.load(std::sync::atomic::Ordering::SeqCst) {
        diagnostic(
            json,
            &anyhow::anyhow!(
                ErrorCode::Unconfirmed
                    .error("upload interrupted; pull the destination before retrying")
            ),
        );
        75
    } else {
        130
    }
}
pub(super) async fn run(id: &Identity, target: &str, command: Remote, json: bool) -> Result<i32> {
    match command {
        Remote::Push {
            local,
            remote,
            cwd,
            mkdir,
            no_overwrite,
            expect,
        } => {
            let input_snapshot;
            let (file, size, hash) = if local == "-" {
                let (temp, size, hash) = crate::transfer::snapshot_input(std::io::stdin().lock())?;
                input_snapshot = temp;
                (input_snapshot.reopen()?, size, hash)
            } else {
                crate::transfer::prepare_upload(Path::new(&local))?
            };
            let mut s = session(id, target).await?;
            let path = remote;
            let sent = std::sync::atomic::AtomicBool::new(false);
            let operation = async {
                sent.store(true, std::sync::atomic::Ordering::SeqCst);
                s.send_request(Request::Push {
                    context: JobContext::new(&s.db_id),
                    path,
                    cwd,
                    size,
                    sha256: hash.clone(),
                    mkdir,
                    no_overwrite,
                    expect,
                })
                .await?;
                let job = s.accept_operation().await?;
                net::send_file(&mut s.ws, &file).await?;
                Ok::<_, anyhow::Error>((response(&mut s.ws).await?, job))
            };
            let result = tokio::select! {
                r=operation=>r,
                _=tokio::signal::ctrl_c()=>return Ok(interrupted_upload(&sent, json)),
                _=termination()=>{return Ok(if sent.load(std::sync::atomic::Ordering::SeqCst){75}else{125});},
            };
            match result {
                Ok((Data::File { path, .. }, job)) => {
                    s.finish().await;
                    print(
                        json,
                        &serde_json::json!({"device_id":target,
                        "remote_path":path,
                        "size":size,
                        "sha256":hash,
                        "job":job_ref(&job),
                        "request_id":job.request_id}),
                        || println!("{path}"),
                    );
                    Ok(0)
                }
                Ok(_) => bail!(ErrorCode::InvalidMessage.error("expected file confirmation")),
                Err(e) => {
                    diagnostic(json, &e);
                    Ok(
                        if net::explicit(&e) || crate::error::is(&e, ErrorCode::DeviceBusy) {
                            125
                        } else if definitive(&e) {
                            1
                        } else {
                            75
                        },
                    )
                }
            }
        }
        Remote::Pull { remote, local, cwd } => {
            let mut s = session(id, target).await?;
            let path = remote.clone();
            s.send_request(Request::Pull {
                context: JobContext::new(&s.db_id),
                path,
                cwd,
            })
            .await?;
            let job = s.accept_operation().await?;
            let Data::File {
                path, size, sha256, ..
            } = response(&mut s.ws).await?
            else {
                bail!(ErrorCode::InvalidMessage.error("expected file header"))
            };
            let mut temp =
                net::receive_file_with_prefix(&mut s.ws, size, &sha256, "xrun-pull-").await?;
            s.finish().await;
            if local.as_deref() == Some("-") {
                std::io::copy(temp.as_file_mut(), &mut std::io::stdout().lock())?;
                return Ok(0);
            }
            let local = if let Some(path) = local {
                crate::transfer::save_local_reader(
                    &std::env::current_dir()?.join(path),
                    temp.as_file_mut(),
                )?
            } else {
                temp.as_file().sync_all()?;
                config::sync_parent(temp.path())?;
                temp.keep()?.1
            };
            print(
                json,
                &serde_json::json!({"path":local,
                        "device_id":target,
                        "remote_path":path,
                        "size":size,
                        "sha256":sha256,
                        "job":job_ref(&job),
                        "request_id":job.request_id}),
                || println!("{}", local.display()),
            );
            Ok(0)
        }
        Remote::Screenshot {
            local,
            max_width,
            format,
            quality,
        } => {
            let options = ScreenshotOptions {
                max_width,
                format,
                quality,
            };
            options.validate()?;
            let mut s = session(id, target).await?;
            s.send_request(Request::Screenshot {
                context: JobContext::new(&s.db_id),
                options,
            })
            .await?;
            let job = s.accept_operation().await?;
            let Data::File {
                size,
                sha256,
                width,
                height,
                captured_at,
                format: actual_format,
                mime,
                ..
            } = response(&mut s.ws).await?
            else {
                bail!(ErrorCode::InvalidMessage.error("expected screenshot header"))
            };
            let bytes = net::receive_bytes(&mut s.ws, size, &sha256, MAX_SCREENSHOT).await?;
            let actual_format = actual_format
                .context(ErrorCode::InvalidMessage.error("screenshot format missing"))?;
            if actual_format != format
                || max_width.is_some_and(|limit| width.is_none_or(|width| width > limit))
            {
                bail!(
                    ErrorCode::InvalidMessage
                        .error("screenshot output differs from the requested format or width")
                );
            }
            if mime.as_deref() != Some(actual_format.mime()) {
                bail!(ErrorCode::InvalidMessage.error("screenshot MIME differs from its format"));
            }
            crate::screenshot::validate_received(
                &bytes,
                actual_format,
                width.context("screenshot width missing")?,
                height.context("screenshot height missing")?,
            )?;
            s.finish().await;
            let path = save_download(
                &bytes,
                local,
                "xrun-screen-",
                &format!(".{}", actual_format.extension()),
            )?;
            print(
                json,
                &serde_json::json!({"path":path,
                        "device_id":target,
                        "width":width,
                        "height":height,
                        "captured_at":captured_at,
                        "format":actual_format, "mime":mime, "size":size, "sha256":sha256,
                        "job":job_ref(&job),
                        "request_id":job.request_id}),
                || println!("{}", path.display()),
            );
            Ok(0)
        }
        _ => bail!(ErrorCode::InvalidRequest.error("command dispatched to the wrong handler")),
    }
}
