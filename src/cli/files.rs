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

fn show_transfer(
    json: bool,
    target: &str,
    result: crate::client::files::FileTransfer,
) -> Result<i32> {
    if result.job.kind() == JobKind::Pull && result.path.is_none() {
        return Ok(0);
    }
    print(
        json,
        &serde_json::json!({
            "path":result.path,"device_id":target,"remote_path":result.remote_path,
            "size":result.size,"sha256":result.sha256,"skipped":result.skipped,
            "resumed_from":result.resumed_from,"job":job_ref(&result.job),"request_id":result.job.request_id
        }),
        || {
            if let Some(path) = &result.path {
                println!("{}", path.display());
            } else {
                println!("{}", result.remote_path);
            }
        },
    );
    Ok(0)
}
pub(super) async fn run(id: &Identity, target: &str, command: Remote, json: bool) -> Result<i32> {
    match operation(id, target, command, json).await {
        Ok(code) => Ok(code),
        Err(error) => {
            diagnostic(json, &error);
            Ok(if crate::error::is(&error, ErrorCode::Unconfirmed) {
                75
            } else if net::explicit(&error) || crate::error::is(&error, ErrorCode::DeviceBusy) {
                125
            } else if definitive(&error) {
                1
            } else {
                75
            })
        }
    }
}
async fn operation(id: &Identity, target: &str, command: Remote, json: bool) -> Result<i32> {
    match command {
        Remote::Push {
            local,
            remote,
            cwd,
            mkdir,
            no_overwrite,
            expect,
        } => {
            let result = crate::client::files::push_with_identity(
                id,
                target,
                &local,
                crate::client::files::UploadOptions {
                    path: remote,
                    cwd,
                    mkdir,
                    no_overwrite,
                    expect,
                },
            )
            .await?;
            show_transfer(json, target, result)
        }
        Remote::Pull { remote, local, cwd } => {
            let result = crate::client::files::pull_with_identity(
                id,
                target,
                &remote,
                local.as_deref().map(Path::new),
                cwd,
            )
            .await?;
            show_transfer(json, target, result)
        }
        Remote::Resume { id: request_id } => {
            let result =
                crate::client::files::resume_with_identity(id, target, &request_id).await?;
            show_transfer(json, target, result)
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
