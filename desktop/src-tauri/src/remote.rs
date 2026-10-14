//! Authorized remote commands; no shell is executed on the desktop host.
use super::{app::Desktop, bridge, error::CommandError};
use serde::Deserialize;
use serde_json::{Value, json};
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;

/// Requested screenshot encoding; here it only names the file being saved.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ScreenshotFormat {
    Png,
    Jpeg,
    Webp,
}
impl ScreenshotFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
        }
    }
}

async fn remote(method: &str, params: Value) -> Result<Value, CommandError> {
    bridge::call(method, params)
        .await
        .map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) async fn choose_screenshot_destination<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    format: ScreenshotFormat,
) -> Result<Option<String>, CommandError> {
    let lightweight = app.state::<super::lightweight::Lightweight>();
    let _dialog = lightweight.dialog();
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(format!("screenshot.{}", format.extension()))
        .add_filter("Image", &[format.extension()])
        .save_file(move |path| {
            let _ = tx.send(path);
        });
    rx.await
        .map_err(CommandError::from_error)?
        .map(|path| {
            path.into_path()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(CommandError::from_error)
        })
        .transpose()
}
#[tauri::command]
pub(super) async fn remote_screenshot(
    state: State<'_, Desktop>,
    device: String,
    path: String,
    options: Value,
) -> Result<Value, CommandError> {
    let _guard = state.action.lock().await;
    let params = json!({"device": device, "path": path, "options": options});
    remote("remote_screenshot", params).await
}

#[tauri::command]
pub(super) async fn remote_submit(
    state: State<'_, Desktop>,
    device: String,
    command: Value,
) -> Result<Value, CommandError> {
    let _guard = state.action.lock().await;
    remote(
        "remote_submit",
        json!({"device": device, "command": command}),
    )
    .await
}

#[tauri::command]
pub(super) async fn remote_recent(device: String) -> Result<Value, CommandError> {
    remote("remote_recent", json!({ "device": device })).await
}

#[tauri::command]
pub(super) async fn remote_query(
    device: String,
    request_id: String,
) -> Result<Value, CommandError> {
    let params = json!({"device": device, "request_id": request_id});
    remote("remote_query", params).await
}

/// The helper bounds each query at 30 seconds and reports CONNECT_TIMEOUT.
#[tauri::command]
pub(super) async fn remote_output(
    device: String,
    db_id: String,
    job_id: String,
    after: u64,
) -> Result<Value, CommandError> {
    let params = json!({"device": device, "db_id": db_id, "job_id": job_id, "after": after});
    remote("remote_output", params).await
}

#[tauri::command]
pub(super) async fn remote_cancel(
    state: State<'_, Desktop>,
    device: String,
    db_id: String,
    job_id: String,
) -> Result<Value, CommandError> {
    let _guard = state.action.lock().await;
    let params = json!({"device": device, "db_id": db_id, "job_id": job_id});
    remote("remote_cancel", params).await
}

#[cfg(test)]
mod tests {
    use super::super::app::Desktop;
    use super::super::commands::tests::{build_app, invoke, isolated};
    use anyhow::Result;
    use serde_json::json;
    use tauri::Manager;

    #[test]
    fn desktop_remote_ipc_validates_the_structured_command_before_creating_a_receipt() -> Result<()>
    {
        isolated(
            "desktop_remote_ipc_validates_the_structured_command_before_creating_a_receipt",
            || {
                let (app, window) = build_app();
                let error = invoke(
                    &window,
                    "remote_submit",
                    json!({"device":"peer1","command":{
                        "request_id":"draft","script":"","shell":"sh","cwd":null,"timeout":30
                    }}),
                )
                .unwrap_err();
                assert_eq!(error["code"], "INVALID_REQUEST");
                assert!(app.state::<Desktop>().error.lock().unwrap().is_none());
                assert!(
                    !xrun::client::services::data_dir()?
                        .join("submissions.sqlite")
                        .exists()
                );
                Ok(())
            },
        )
    }
}

#[tauri::command]
pub(super) async fn choose_transfer_file<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    upload: bool,
    name: String,
) -> Result<Option<String>, CommandError> {
    let lightweight = app.state::<super::lightweight::Lightweight>();
    let _dialog = lightweight.dialog();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let dialog = app.dialog().file().set_file_name(name);
    let chosen = move |path| {
        let _ = tx.send(path);
    };
    if upload {
        dialog.pick_file(chosen);
    } else {
        dialog.save_file(chosen);
    }
    rx.await
        .map_err(CommandError::from_error)?
        .map(|path| {
            path.into_path()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(CommandError::from_error)
        })
        .transpose()
}
#[tauri::command]
pub(super) async fn remote_file_receipts(device: String) -> Result<Value, CommandError> {
    remote("remote_file_receipts", json!({ "device": device })).await
}
#[tauri::command]
pub(super) async fn remote_file_upload(
    state: State<'_, Desktop>,
    device: String,
    local: String,
    options: Value,
) -> Result<Value, CommandError> {
    let _guard = state.action.lock().await;
    let params = json!({"device": device, "local": local, "options": options});
    remote("remote_file_upload", params).await
}
#[tauri::command]
pub(super) async fn remote_file_download(
    state: State<'_, Desktop>,
    device: String,
    remote: String,
    local: String,
    cwd: Option<String>,
) -> Result<Value, CommandError> {
    let _guard = state.action.lock().await;
    let params = json!({"device": device, "remote": remote, "local": local, "cwd": cwd});
    self::remote("remote_file_download", params).await
}
#[tauri::command]
pub(super) async fn remote_file_resume(
    state: State<'_, Desktop>,
    device: String,
    request_id: String,
) -> Result<Value, CommandError> {
    let _guard = state.action.lock().await;
    let params = json!({"device": device, "request_id": request_id});
    remote("remote_file_resume", params).await
}
#[tauri::command]
pub(super) async fn remote_file_cancel(
    state: State<'_, Desktop>,
    device: String,
    request_id: String,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    let params = json!({"device": device, "request_id": request_id});
    remote("remote_file_cancel", params).await.map(drop)
}
