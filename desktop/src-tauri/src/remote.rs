//! Authorized remote commands; no shell is executed on the desktop host.
use super::{app::Desktop, error::CommandError};
use tauri::State;
use xrun::{
    client::remote::{self, CommandRequest, Output, Receipt},
    protocol::Job,
};

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
