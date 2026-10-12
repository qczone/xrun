//! Link connection and Cloudflare deployment adapters; browser authorization is cancellable.
use super::{
    app::{Desktop, record},
    error::{self, CommandError},
    platform,
};
use tauri::{Emitter, Manager, State};
use xrun::client::relay::{Account, DeployedRelay};

#[tauri::command]
pub(super) fn connection_defaults() -> String {
    xrun::client::suggested_name()
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
        xrun::client::connect(link.trim(), Some(name.trim().to_string()), allow_manager).await?;
        xrun::client::services::initialize_daemon()
            .map_err(|e| error::failure("SERVICE_START_FAILED", format!("{e:#}")))?;
        platform::start()
            .await
            .map_err(|e| error::failure("SERVICE_START_FAILED", format!("{e:#}")))
    }
    .await;
    record(&app, result)
}

fn authorization<R: tauri::Runtime>(app: &tauri::AppHandle<R>, url: &str) {
    *app.state::<Desktop>()
        .cloudflare_authorization
        .lock()
        .unwrap() = Some(url.to_string());
    let _ = app.emit("xrun-cloudflare-authorization", url);
}

#[tauri::command]
pub(super) async fn cloudflare_accounts<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    reauthorize: bool,
) -> Result<Option<Vec<Account>>, CommandError> {
    let _guard = state.action.lock().await;
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    *state.cloudflare_cancel.lock().unwrap() = Some(cancel);
    let prompt = |url: &str| authorization(&app, url);
    let result = tokio::select! {
        result = xrun::client::relay::cloudflare_accounts(&prompt, reauthorize) => result.map(Some),
        _ = cancelled => Ok(None),
    };
    state.cloudflare_cancel.lock().unwrap().take();
    state.cloudflare_authorization.lock().unwrap().take();
    record(&app, result)
}

#[tauri::command]
pub(super) fn cancel_cloudflare_authorization(state: State<'_, Desktop>) {
    if let Some(cancel) = state.cloudflare_cancel.lock().unwrap().take() {
        let _ = cancel.send(());
    }
}

#[tauri::command]
pub(super) fn open_cloudflare_authorization(state: State<'_, Desktop>) -> Result<(), CommandError> {
    let url = state.cloudflare_authorization.lock().unwrap();
    let Some(url) = url.as_deref() else {
        return Err(CommandError {
            code: "INVALID_LINK".into(),
            message: "authorization is no longer pending".into(),
        });
    };
    xrun::client::relay::open_cloudflare_authorization(url).map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) async fn deploy_cloudflare<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    name: String,
    account: String,
) -> Result<DeployedRelay, CommandError> {
    let _guard = state.action.lock().await;
    let prompt = |url: &str| authorization(&app, url);
    let result = xrun::client::relay::deploy_cloudflare(name.trim(), account, &prompt).await;
    state.cloudflare_authorization.lock().unwrap().take();
    record(&app, result)
}

#[tauri::command]
pub(super) fn cloudflare_relays() -> Result<Vec<DeployedRelay>, CommandError> {
    xrun::client::relay::cloudflare_relays().map_err(CommandError::from_error)
}

#[cfg(test)]
mod tests {
    use super::super::commands::tests::{build_app, invoke, isolated};
    use super::*;
    use anyhow::Result;
    use serde_json::json;

    #[test]
    fn desktop_connection_ipc_validates_consent_and_reads_deployments_without_creating_an_identity()
    -> Result<()> {
        isolated(
            "desktop_connection_ipc_validates_consent_and_reads_deployments_without_creating_an_identity",
            || {
                let (app, window) = build_app();
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
                let (sender, mut receiver) = tokio::sync::oneshot::channel();
                *app.state::<Desktop>().cloudflare_cancel.lock().unwrap() = Some(sender);
                assert_eq!(
                    invoke(&window, "cancel_cloudflare_authorization", json!({})),
                    Ok(json!(null))
                );
                assert_eq!(receiver.try_recv(), Ok(()));
                let expired =
                    invoke(&window, "open_cloudflare_authorization", json!({})).unwrap_err();
                assert_eq!(expired["code"], "INVALID_LINK");
                Ok(())
            },
        )
    }
}
