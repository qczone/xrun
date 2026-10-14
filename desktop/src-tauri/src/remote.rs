//! Authorized remote commands; no shell is executed on the desktop host.
use super::{app::Desktop, error::CommandError};
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;
use xrun::{
    client::remote::{self, CommandRequest, Output, Receipt},
    protocol::{Job, ScreenshotFormat, ScreenshotOptions},
};

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
    options: ScreenshotOptions,
) -> Result<remote::ScreenshotDownload, CommandError> {
    let _guard = state.action.lock().await;
    remote::screenshot(&device, std::path::Path::new(&path), options)
        .await
        .map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) async fn remote_submit(
    state: State<'_, Desktop>,
    device: String,
    command: CommandRequest,
) -> Result<Job, CommandError> {
    let _guard = state.action.lock().await;
    remote::submit(&device, command)
        .await
        .map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) fn remote_recent(device: String) -> Result<Vec<Receipt>, CommandError> {
    remote::recent(&device).map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) async fn remote_query(device: String, request_id: String) -> Result<Job, CommandError> {
    remote::query(&device, &request_id)
        .await
        .map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) async fn remote_output(
    device: String,
    db_id: String,
    job_id: String,
    after: u64,
) -> Result<Output, CommandError> {
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        remote::output(&device, &db_id, &job_id, after),
    )
    .await
    .map_err(|_| CommandError {
        code: "CONNECT_TIMEOUT".into(),
        message: "task output query timed out".into(),
    })?
    .map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) async fn remote_cancel(
    state: State<'_, Desktop>,
    device: String,
    db_id: String,
    job_id: String,
) -> Result<Job, CommandError> {
    let _guard = state.action.lock().await;
    remote::cancel(&device, &db_id, &job_id)
        .await
        .map_err(CommandError::from_error)
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
pub(super) fn remote_file_receipts(
    device: String,
) -> Result<Vec<xrun::client::files::TransferReceipt>, CommandError> {
    xrun::client::files::recent(&device).map_err(CommandError::from_error)
}
#[tauri::command]
pub(super) async fn remote_file_upload(
    state: State<'_, Desktop>,
    device: String,
    local: String,
    options: xrun::client::files::UploadOptions,
) -> Result<xrun::client::files::FileTransfer, CommandError> {
    let _guard = state.action.lock().await;
    xrun::client::files::push(&device, std::path::Path::new(&local), options)
        .await
        .map_err(CommandError::from_error)
}
#[tauri::command]
pub(super) async fn remote_file_download(
    state: State<'_, Desktop>,
    device: String,
    remote: String,
    local: String,
    cwd: Option<String>,
) -> Result<xrun::client::files::FileTransfer, CommandError> {
    let _guard = state.action.lock().await;
    xrun::client::files::pull(&device, &remote, Some(std::path::Path::new(&local)), cwd)
        .await
        .map_err(CommandError::from_error)
}
#[tauri::command]
pub(super) async fn remote_file_resume(
    state: State<'_, Desktop>,
    device: String,
    request_id: String,
) -> Result<xrun::client::files::FileTransfer, CommandError> {
    let _guard = state.action.lock().await;
    xrun::client::files::resume(&device, &request_id)
        .await
        .map_err(CommandError::from_error)
}
#[tauri::command]
pub(super) async fn remote_file_cancel(
    state: State<'_, Desktop>,
    device: String,
    request_id: String,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    xrun::client::files::cancel(&device, &request_id)
        .await
        .map_err(CommandError::from_error)
}
