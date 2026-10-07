//! File and screenshot operations with admission limits and audit updates.
use crate::error::ErrorCode;
use crate::{
    net::{self, Ws},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use std::{path::PathBuf, sync::Arc};

use super::{FileAudit, Runtime};

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
            let permit = rt
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
            let mut owned_audit = audit.take();
            let path = crate::transfer::push(
                path,
                contents,
                mkdir,
                no_overwrite,
                expect,
                move |path, published| {
                    let _permit = permit;
                    if let Some(mut a) = owned_audit.take() {
                        a.completed = published;
                        a.value["path"] = serde_json::json!(path);
                        let value = a.snapshot();
                        a.store.audit(value)?;
                        a.persisted = true;
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
        Request::Pull { path, cwd } => {
            let permit = rt
                .files
                .clone()
                .try_acquire_owned()
                .context(ErrorCode::DeviceBusy.error("too many file operations"))?;
            let path = crate::transfer::remote_path(&path, &operation_cwd(&rt, cwd)?)?;
            let (contents, size, hash, _permit) =
                crate::transfer::snapshot(path.clone(), permit).await?;
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
            let permit = rt
                .files
                .clone()
                .try_acquire_owned()
                .context(ErrorCode::DeviceBusy.error("too many file operations"))?;
            let (capture, _permit) = crate::screenshot::capture_with_permit(permit).await?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_tungstenite::tungstenite::protocol::Role;

    #[tokio::test]
    async fn screenshot_wire_metadata_and_audit_agree_and_disconnects_are_not_successes()
    -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = Arc::new(crate::store::TaskStore::open(
            &temp.path().join("tasks.db"),
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
            let mut audit = Some(FileAudit {
                store: store.clone(),
                value: serde_json::json!({"op":"screenshot"}),
                completed: false,
                stream_counts: None,
                persisted: false,
            });
            let capture = crate::screenshot::Capture {
                bytes: png.clone(),
                width: 1,
                height: 1,
                at: "2026-10-06T00:00:00Z".into(),
            };
            if disconnected {
                drop(client);
                assert!(
                    send_capture(&mut server, capture, &mut audit)
                        .await
                        .is_err()
                );
                assert!(!audit.as_ref().unwrap().completed);
            } else {
                send_capture(&mut server, capture, &mut audit).await?;
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
                    net::receive_bytes(&mut client, size, &hash, MAX_FILE).await?,
                    png
                );
                assert!(audit.as_ref().unwrap().completed);
            }
            assert_eq!(audit.as_ref().unwrap().value["size"], png.len());
            drop(audit);
            store.flush().await?;
        }
        let db = rusqlite::Connection::open(temp.path().join("tasks.db"))?;
        let results = db
            .prepare("SELECT json_extract(data,'$.result') FROM audit ORDER BY rowid")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        assert_eq!(results, ["ok", "failed_or_disconnected"]);
        Ok(())
    }
}
