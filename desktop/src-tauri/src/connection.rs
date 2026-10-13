//! Link connection and Cloudflare deployment adapters; browser authorization is cancellable.
use super::{
    app::{Desktop, record},
    error::{self, CommandError},
    platform,
};
use tauri::{Emitter, Manager, State};
use xrun::client::relay::{Account, DeployedRelay};
use xrun::client::relay::{
    DeploymentReporter, DeploymentStage as Stage, ProgressDetails, ProgressStatus,
};

pub(super) fn progress<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    id: Option<String>,
    operation: &str,
) -> DeploymentReporter {
    let handle = app.clone();
    let reporter = DeploymentReporter::new(
        id.unwrap_or_else(|| format!("desktop-{}", xrun::protocol::now_ms())),
        "cloudflare",
        operation,
        move |event| {
            if event.stage == Stage::AuthorizationReceived {
                handle
                    .state::<Desktop>()
                    .cloudflare_cancel
                    .lock()
                    .unwrap()
                    .take();
                handle
                    .state::<Desktop>()
                    .cloudflare_authorization
                    .lock()
                    .unwrap()
                    .take();
                let window = handle.clone();
                let _ = handle.run_on_main_thread(move || super::tray::present(&window));
            }
            let _ = handle.emit("xrun-deployment-progress", event);
        },
    )
    .with_interactive(false);
    match xrun::client::services::data_dir() {
        Ok(directory) => reporter.with_log(directory.join("deployment.log")),
        Err(_) => reporter,
    }
}

async fn connect_relay(
    relay: &DeployedRelay,
    name: &str,
    reporter: &DeploymentReporter,
) -> anyhow::Result<()> {
    xrun::client::relay::check_relay(&relay.link).await?;
    reporter
        .step(
            Stage::NetworkCreation,
            xrun::client::connect(&relay.link, Some(name.to_owned()), false),
        )
        .await?;
    reporter
        .step(Stage::ServiceStart, async {
            xrun::client::services::initialize_daemon()?;
            platform::start().await
        })
        .await
        .map_err(|e| error::failure("SERVICE_START_FAILED", format!("{e:#}")))?;
    reporter
        .step(Stage::ServiceConnection, async {
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                loop {
                    let status = xrun::client::local_status()?;
                    if status.daemon_connected == Some(true) {
                        return Ok::<_, anyhow::Error>(());
                    }
                    if !status.daemon_running {
                        anyhow::bail!(error::failure(
                            "SERVICE_FAILED",
                            "the service stopped before connecting"
                        ));
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            })
            .await
            .map_err(|_| {
                error::failure(
                    "CONNECT_TIMEOUT",
                    "network created; retry starting the service to connect",
                )
            })?
        })
        .await
        .map_err(|e| error::failure("SERVICE_START_FAILED", format!("{e:#}")))
}

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
    remember: Option<bool>,
    progress_id: Option<String>,
    finish_temporary: Option<bool>,
) -> Result<Option<Vec<Account>>, CommandError> {
    let _guard = state.action.lock().await;
    let (cancel, cancelled) = tokio::sync::oneshot::channel();
    *state.cloudflare_cancel.lock().unwrap() = Some(cancel);
    let prompt = |url: &str| authorization(&app, url);
    let reporter = progress(&app, progress_id, "cloudflare_accounts");
    let result = reporter.scope(async { tokio::select! {
        result = xrun::client::relay::cloudflare_accounts(&prompt, reauthorize, remember.unwrap_or(false)) => {
            let accounts = result?;
            if finish_temporary.unwrap_or(false) {
                xrun::client::relay::end_cloudflare_authorization().await?;
            }
            Ok(Some(accounts))
        },
        Ok(()) = cancelled => Ok(None),
    }}).await;
    reporter.report(
        Stage::Complete,
        match &result {
            Ok(Some(_)) => ProgressStatus::Completed,
            Ok(None) => ProgressStatus::Cancelled,
            Err(_) => ProgressStatus::Failed,
        },
        0,
        ProgressDetails {
            error: result.as_ref().err().map(xrun::protocol::Data::error),
            ..Default::default()
        },
    );
    state.cloudflare_cancel.lock().unwrap().take();
    state.cloudflare_authorization.lock().unwrap().take();
    record(&app, result)
}

#[tauri::command]
pub(super) async fn cancel_cloudflare_authorization(
    state: State<'_, Desktop>,
) -> Result<(), CommandError> {
    let account_selection = state.cloudflare_cancel.lock().unwrap().take();
    let selecting = account_selection.is_some();
    if let Some(cancel) = account_selection {
        let _ = cancel.send(());
    }
    let pending = xrun::client::relay::cancel_pending_cloudflare_authorization();
    if !selecting && !pending && state.cloudflare_authorization.lock().unwrap().is_some() {
        return Err(CommandError::from_error(error::failure(
            "INVALID_REQUEST",
            "authorization completed; deployment continues",
        )));
    }
    xrun::client::relay::end_cloudflare_authorization()
        .await
        .map_err(CommandError::from_error)
}

#[tauri::command]
pub(super) fn cloudflare_remembered_login() -> Result<bool, CommandError> {
    xrun::client::relay::cloudflare_remembered_login().map_err(CommandError::from_error)
}
#[tauri::command]
pub(super) async fn logout_cloudflare<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    progress_id: Option<String>,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    let reporter = progress(&app, progress_id, "logout_cloudflare");
    let result = reporter
        .scope(xrun::client::relay::logout_cloudflare())
        .await;
    reporter.report(
        Stage::Complete,
        if result.is_ok() {
            ProgressStatus::Completed
        } else {
            ProgressStatus::Failed
        },
        0,
        ProgressDetails {
            error: result.as_ref().err().map(xrun::protocol::Data::error),
            ..Default::default()
        },
    );
    record(&app, result)
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
#[allow(clippy::too_many_arguments)]
pub(super) async fn create_cloudflare_network<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    worker_name: String,
    account: String,
    device_name: String,
    region: Option<xrun::client::relay::Region>,
    resume: bool,
    progress_id: String,
) -> Result<Option<DeployedRelay>, CommandError> {
    let _guard = state.action.lock().await;
    let reporter = progress(&app, Some(progress_id), "create_cloudflare_network");
    let prompt = |url: &str| authorization(&app, url);
    let result = reporter
        .scope(async {
            let prepared = if resume {
                xrun::client::relay::saved_cloudflare_relay(worker_name.trim())
            } else {
                xrun::client::relay::prepare_cloudflare_relay(
                    worker_name.trim(),
                    account,
                    &prompt,
                    region,
                )
                .await
            };
            let relay = match prepared {
                Ok(relay) => relay,
                Err(primary) => {
                    if let Err(cleanup) = xrun::client::relay::end_cloudflare_authorization().await
                    {
                        reporter.report(
                            Stage::AuthorizationCleanup,
                            ProgressStatus::Warning,
                            0,
                            ProgressDetails {
                                error: Some(xrun::protocol::Data::error(&cleanup)),
                                ..Default::default()
                            },
                        );
                    }
                    return Err(primary);
                }
            };
            reporter.report(
                Stage::Deployed,
                ProgressStatus::Completed,
                0,
                ProgressDetails {
                    deployment: Some(relay.clone()),
                    ..Default::default()
                },
            );
            // Keep both futures alive: connection proceeds while revocation completes,
            // and no credential-cleanup task is abandoned when a CLI/App future returns.
            settle_connection(
                connect_relay(&relay, device_name.trim(), &reporter),
                xrun::client::relay::end_cloudflare_authorization(),
                &reporter,
            )
            .await?;
            Ok(relay)
        })
        .await;
    reporter.report(
        Stage::Complete,
        if result.is_ok() {
            ProgressStatus::Completed
        } else if result
            .as_ref()
            .err()
            .is_some_and(|e| xrun::error::is(e, xrun::error::ErrorCode::OAuthCancelled))
        {
            ProgressStatus::Cancelled
        } else {
            ProgressStatus::Failed
        },
        0,
        ProgressDetails {
            error: result.as_ref().err().map(xrun::protocol::Data::error),
            ..Default::default()
        },
    );
    state.cloudflare_authorization.lock().unwrap().take();
    if result
        .as_ref()
        .err()
        .is_some_and(|e| xrun::error::is(e, xrun::error::ErrorCode::OAuthCancelled))
    {
        return record(&app, Ok(None));
    }
    record(&app, result.map(Some))
}

async fn settle_connection(
    connected: impl std::future::Future<Output = anyhow::Result<()>>,
    cleanup: impl std::future::Future<Output = anyhow::Result<()>>,
    reporter: &DeploymentReporter,
) -> anyhow::Result<()> {
    let (connected, cleanup) = tokio::join!(connected, cleanup);
    if let Err(primary) = connected {
        if let Err(cleanup) = cleanup {
            reporter.report(
                Stage::AuthorizationCleanup,
                ProgressStatus::Warning,
                0,
                ProgressDetails {
                    error: Some(xrun::protocol::Data::error(&cleanup)),
                    ..Default::default()
                },
            );
        }
        return Err(primary);
    }
    cleanup.map_err(|failure| {
        xrun::error::ErrorCode::NetworkCreatedCleanupFailed
            .error(format!("network created; {failure:#}"))
            .into()
    })
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

    #[tokio::test]
    async fn connection_runs_before_cleanup_finishes_and_primary_errors_are_preserved() -> Result<()>
    {
        let reporter = DeploymentReporter::new("owned-test".into(), "cloudflare", "create", |_| {});
        let connected = std::sync::Arc::new(tokio::sync::Notify::new());
        let reached = connected.clone();
        let (release, wait_release) = tokio::sync::oneshot::channel();
        let copy = reporter.clone();
        let task = tokio::spawn(async move {
            settle_connection(
                async {
                    reached.notify_one();
                    Ok(())
                },
                async {
                    wait_release.await?;
                    Ok(())
                },
                &copy,
            )
            .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(2), connected.notified()).await?;
        assert!(!task.is_finished());
        release.send(()).unwrap();
        task.await??;
        let primary =
            xrun::error::ErrorCode::ConnectTimeout.error("owned connection fixture failed");
        let cleanup =
            xrun::error::ErrorCode::OAuthRevocationFailed.error("owned cleanup fixture failed");
        let result = settle_connection(
            async { Err(primary.into()) },
            async { Err(cleanup.into()) },
            &reporter,
        )
        .await;
        assert!(xrun::error::is(
            &result.unwrap_err(),
            xrun::error::ErrorCode::ConnectTimeout
        ));
        let result = settle_connection(
            async { Ok(()) },
            async {
                Err(xrun::error::ErrorCode::OAuthRevocationFailed
                    .error("owned cleanup fixture failed")
                    .into())
            },
            &reporter,
        )
        .await;
        assert!(xrun::error::is(
            &result.unwrap_err(),
            xrun::error::ErrorCode::NetworkCreatedCleanupFailed
        ));
        Ok(())
    }

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
                let error = invoke(
                    &window,
                    "cloudflare_accounts",
                    json!({"reauthorize":true,"remember":true}),
                )
                .unwrap_err();
                assert_eq!(error["code"], "NOT_MANAGER");
                assert!(!directory.join("cloudflare/credentials.toml").exists());
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
