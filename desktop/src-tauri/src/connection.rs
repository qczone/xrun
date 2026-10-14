//! Link connection and Cloudflare deployment adapters. The flows run in the helper,
//! which keeps browser authorization cancellable and reports progress here.
use super::{
    app::{Desktop, record},
    bridge,
    error::{self, CommandError},
    platform,
};
use serde_json::{Value, json};
use tauri::{Emitter, State};

/// Forward one operation's progress to the webview. A returned browser authorization
/// brings the window forward, so setup continues where the user left it.
pub(super) fn deployment_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event: &str,
    data: Value,
) {
    match event {
        "deployment_progress" => {
            if data["stage"] == "authorization_received" {
                let window = app.clone();
                let _ = app.run_on_main_thread(move || super::tray::present(&window));
            }
            let _ = app.emit("xrun-deployment-progress", data);
        }
        "cloudflare_authorization" => {
            let _ = app.emit("xrun-cloudflare-authorization", data);
        }
        _ => {}
    }
}

fn events<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> impl Fn(&str, Value) + Send + Sync + 'static {
    let app = app.clone();
    move |event: &str, data: Value| deployment_event(&app, event, data)
}

#[tauri::command]
pub(super) async fn connection_defaults() -> Result<String, CommandError> {
    bridge::get("suggested_name", json!({}))
        .await
        .map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) async fn connect<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    link: String,
    name: String,
    allow_manager: bool,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    let result = async {
        let params =
            json!({"link": link.trim(), "name": name.trim(), "allow_manager": allow_manager});
        bridge::call("connect", params).await?;
        bridge::call("initialize_daemon", json!({}))
            .await
            .map_err(|e| error::failure("SERVICE_START_FAILED", format!("{e:#}")))?;
        platform::start()
            .await
            .map_err(|e| error::failure("SERVICE_START_FAILED", format!("{e:#}")))
    }
    .await;
    record(&app, result)
}

#[tauri::command]
pub(super) async fn cloudflare_accounts<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    reauthorize: bool,
    remember: Option<bool>,
    progress_id: Option<String>,
    finish_temporary: Option<bool>,
) -> Result<Value, CommandError> {
    let _guard = state.action.lock().await;
    let params = json!({
        "reauthorize": reauthorize,
        "remember": remember,
        "progress_id": progress_id,
        "finish_temporary": finish_temporary,
    });
    let result = bridge::call_with_events("cloudflare_accounts", params, events(&app)).await;
    record(&app, result)
}

#[tauri::command]
pub(super) async fn cancel_cloudflare_authorization() -> Result<(), CommandError> {
    bridge::call("cancel_cloudflare_authorization", json!({}))
        .await
        .map(drop)
        .map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) async fn cloudflare_remembered_login() -> Result<bool, CommandError> {
    bridge::get("cloudflare_remembered_login", json!({}))
        .await
        .map_err(CommandError::from_error)
}
#[tauri::command]
pub(super) async fn logout_cloudflare<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    progress_id: Option<String>,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    let params = json!({ "progress_id": progress_id });
    let result = bridge::call_with_events("logout_cloudflare", params, events(&app)).await;
    record(&app, result.map(drop))
}

#[tauri::command]
pub(super) async fn open_cloudflare_authorization() -> Result<(), CommandError> {
    bridge::call("open_cloudflare_authorization", json!({}))
        .await
        .map(drop)
        .map_err(CommandError::from_error)
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub(super) async fn create_cloudflare_network<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    worker_name: String,
    account: String,
    device_name: String,
    region: Option<Value>,
    resume: bool,
    progress_id: String,
) -> Result<Value, CommandError> {
    let _guard = state.action.lock().await;
    let params = json!({
        "worker_name": worker_name,
        "account": account,
        "device_name": device_name,
        "region": region,
        "resume": resume,
        "progress_id": progress_id,
    });
    // An OAuth cancellation returns null, which clears the previous error.
    let result = bridge::call_with_events("create_cloudflare_network", params, events(&app)).await;
    record(&app, result)
}

#[tauri::command]
pub(super) async fn cloudflare_relays() -> Result<Value, CommandError> {
    bridge::call("cloudflare_relays", json!({}))
        .await
        .map_err(CommandError::from_error)
}

#[cfg(test)]
mod tests {
    use super::super::commands::tests::{build_app, invoke, isolated};
    use anyhow::Result;
    use serde_json::json;

    #[test]
    fn desktop_connection_ipc_validates_consent_and_reads_deployments_without_creating_an_identity()
    -> Result<()> {
        isolated(
            "desktop_connection_ipc_validates_consent_and_reads_deployments_without_creating_an_identity",
            || {
                let (_app, window) = build_app();
                let name = invoke(&window, "connection_defaults", json!({})).unwrap();
                assert!(xrun::protocol::valid_name(name.as_str().unwrap()));
                let error = invoke(
                    &window,
                    "connect",
                    json!({"link":"incomplete", "name":"desktop1", "allowManager":false}),
                )
                .unwrap_err();
                assert_eq!(error["code"], "INVALID_RELAY");
                assert_eq!(
                    invoke(&window, "cloudflare_relays", json!({})),
                    Ok(json!([]))
                );
                let directory = xrun::client::services::data_dir()?;
                assert!(!directory.join("identity.toml").exists());
                assert!(!directory.join("cloudflare").exists());
                let error = invoke(
                    &window,
                    "cloudflare_accounts",
                    json!({"reauthorize":true,"remember":true}),
                )
                .unwrap_err();
                assert_eq!(error["code"], "NOT_MANAGER");
                assert!(!directory.join("cloudflare/credentials.toml").exists());
                // Nothing is pending, so cancelling only ends any temporary session.
                assert_eq!(
                    invoke(&window, "cancel_cloudflare_authorization", json!({})),
                    Ok(json!(null))
                );
                let expired =
                    invoke(&window, "open_cloudflare_authorization", json!({})).unwrap_err();
                assert_eq!(expired["code"], "INVALID_LINK");
                Ok(())
            },
        )
    }
}
