//! File and screenshot operations with admission limits and audit updates.
use crate::error::ErrorCode;
use crate::{
    net::{self, Ws},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use std::{path::PathBuf, sync::Arc};

use super::{FileAudit, Runtime};

#[cfg(test)]
mod tests;

pub(super) async fn serve(
    rt: Arc<Runtime>,
    source: &str,
    generation: u64,
    ws: &mut Ws,
    request: Request,
    audit: &mut Option<FileAudit>,
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
        } => {
            let _permit = rt
                .files
                .clone()
                .try_acquire_owned()
                .context(ErrorCode::DeviceBusy.error("too many file operations"))?;
            if no_overwrite && expect.is_some() {
                bail!(ErrorCode::InvalidRequest.error("expect conflicts with no-overwrite"))
            }
            let contents = net::receive_file(ws, size, &sha256).await?;
            rt.check_session(source, generation)?;
            if let Some(a) = audit.as_mut() {
                a.value["size"] = serde_json::json!(size);
            }
            let path = crate::transfer::remote_path(&path, &operation_cwd(&rt, cwd)?)?;
            let path = crate::transfer::push(path, contents, mkdir, no_overwrite, expect).await?;
            if let Some(a) = audit.as_mut() {
                a.completed = true;
                a.value["path"] = serde_json::json!(path);
            }
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
        Request::Pull { path, cwd } => {
            let _permit = rt
                .files
                .clone()
                .try_acquire_owned()
                .context(ErrorCode::DeviceBusy.error("too many file operations"))?;
            let path = crate::transfer::remote_path(&path, &operation_cwd(&rt, cwd)?)?;
            let (contents, size, hash) = crate::transfer::snapshot(path.clone()).await?;
            if let Some(a) = audit.as_mut() {
                a.value["size"] = serde_json::json!(size);
                a.value["path"] = serde_json::json!(path);
            }
            net::send(
                ws,
                &Data::File {
                    path: path.to_string_lossy().into(),
                    size,
                    sha256: hash,
                    width: None,
                    height: None,
                    captured_at: None,
                },
            )
            .await?;
            net::send_file(ws, contents.as_file()).await?;
            if let Some(a) = audit.as_mut() {
                a.completed = true;
            }
        }
        Request::Screenshot => {
            let _permit = rt
                .files
                .clone()
                .try_acquire_owned()
                .context(ErrorCode::DeviceBusy.error("too many file operations"))?;
            let capture = crate::screenshot::capture().await?;
            send_capture(ws, capture, audit).await?;
        }
        _ => bail!(ErrorCode::InvalidRequest.error("operation dispatched to the wrong handler")),
    }
    Ok(())
}

async fn send_capture(
    ws: &mut Ws,
    capture: crate::screenshot::Capture,
    audit: &mut Option<FileAudit>,
) -> Result<()> {
    if let Some(a) = audit.as_mut() {
        a.value["size"] = serde_json::json!(capture.bytes.len());
        a.value["captured_at"] = serde_json::json!(capture.at);
    }
    net::send(
        ws,
        &Data::File {
            path: String::new(),
            size: capture.bytes.len() as u64,
            sha256: sha256(&capture.bytes),
            width: Some(capture.width),
            height: Some(capture.height),
            captured_at: Some(capture.at),
        },
    )
    .await?;
    net::send_bytes(ws, &capture.bytes).await?;
    if let Some(a) = audit.as_mut() {
        a.completed = true;
    }
    Ok(())
}
fn operation_cwd(rt: &Runtime, cwd: Option<String>) -> Result<PathBuf> {
    let path = cwd.map(PathBuf::from).unwrap_or(rt.cwd()?);
    if !path.is_absolute() {
        bail!(ErrorCode::InvalidCwd.error("-C must be absolute"))
    }
    Ok(path)
}
