#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod error;
mod network;
use error::CommandError;
mod platform;

use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::{
    Manager, State,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::DialogExt;

#[derive(Default)]
struct Desktop {
    action: tokio::sync::Mutex<()>,
    error: Mutex<Option<CommandError>>,
}

#[derive(Serialize)]
struct Status {
    local: xrun::client::LocalStatus,
    network: Option<network::NetworkStatus>,
    service: platform::ServiceStatus,
    allow_from: Vec<String>,
    deny_from: Vec<String>,
    error: Option<CommandError>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionSettings {
    default_cwd: Option<std::path::PathBuf>,
    max_concurrent_jobs: usize,
    path: Option<String>,
}

#[derive(Serialize)]
struct Settings {
    execution: ExecutionSettings,
    home_dir: std::path::PathBuf,
    data_dir: std::path::PathBuf,
    os: &'static str,
}

#[tauri::command]
fn settings() -> Result<Settings, CommandError> {
    let result = (|| {
        let cfg = xrun::config::DaemonConfig::load()?;
        Ok::<_, anyhow::Error>(Settings {
            execution: ExecutionSettings {
                default_cwd: cfg.default_cwd,
                max_concurrent_jobs: cfg.max_concurrent_jobs,
                path: cfg
                    .env
                    .iter()
                    .find(|(key, _)| {
                        if cfg!(windows) {
                            key.eq_ignore_ascii_case("PATH")
                        } else {
                            key.as_str() == "PATH"
                        }
                    })
                    .map(|(_, value)| value.clone()),
            },
            home_dir: xrun::config::home_dir()?,
            data_dir: xrun::config::device_dir()?,
            os: std::env::consts::OS,
        })
    })();
    result.map_err(CommandError::from_error)
}

#[tauri::command]
async fn save_settings<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    execution: ExecutionSettings,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    let result = (|| {
        xrun::config::Identity::load()?;
        xrun::config::update_execution(
            execution.default_cwd,
            execution.max_concurrent_jobs,
            execution.path,
        )
    })();
    record(&app, result)
}

#[tauri::command]
async fn choose_directory<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<Option<String>, CommandError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择默认工作目录")
        .pick_folder(move |path| {
            let _ = tx.send(path);
        });
    let path = rx.await.map_err(CommandError::from_error)?;
    path.map(|p| {
        p.into_path()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(CommandError::from_error)
    })
    .transpose()
}

fn local_status<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> anyhow::Result<Status> {
    let mut local = xrun::client::local_status()?;
    let service = platform::status()?;
    local.daemon_installed = service.installed || service.legacy_installed;
    let policy = xrun::config::DaemonConfig::load()?;
    let mut error = app.state::<Desktop>().error.lock().unwrap().clone();
    let network = if local.joined {
        match network::local_status() {
            Ok(value) => Some(value),
            Err(e) => {
                error = Some(CommandError::from_error(e));
                None
            }
        }
    } else {
        None
    };
    Ok(Status {
        local,
        network,
        service,
        allow_from: policy.allow_from,
        deny_from: policy.deny_from,
        error,
    })
}

fn present<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(tray) = app.tray_by_id("xrun") {
        let _ = tray.set_visible(true);
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn record<T, R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    result: anyhow::Result<T>,
) -> Result<T, CommandError> {
    let result = result.map_err(CommandError::from_error);
    let error = result.as_ref().err().cloned();
    *app.state::<Desktop>().error.lock().unwrap() = error.clone();
    match error {
        Some(e) => {
            present(app);
            Err(e)
        }
        None => result,
    }
}

// Joining/creation has completed at this point; the UI can offer a service retry
// without asking the user to repeat registration or retain the invitation link.
fn service_start_error(error: anyhow::Error) -> anyhow::Error {
    error::failure("SERVICE_START_FAILED", format!("{error:#}"))
}

#[tauri::command]
fn status<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<Status, CommandError> {
    local_status(&app).map_err(CommandError::from_error)
}

#[tauri::command]
async fn devices() -> Result<xrun::client::Status, CommandError> {
    xrun::client::status()
        .await
        .map_err(CommandError::from_error)
}

#[tauri::command]
async fn task_history(
    before: Option<i64>,
    filter: String,
) -> Result<xrun::history::TaskPage, CommandError> {
    tauri::async_runtime::spawn_blocking(move || xrun::history::tasks(before, &filter))
        .await
        .map_err(CommandError::from_error)?
        .map_err(CommandError::from_error)
}

#[tauri::command]
async fn task_output(
    db_id: String,
    job: String,
    after: Option<u64>,
) -> Result<xrun::history::TaskOutput, CommandError> {
    tauri::async_runtime::spawn_blocking(move || xrun::history::output(&db_id, &job, after))
        .await
        .map_err(CommandError::from_error)?
        .map_err(CommandError::from_error)
}

#[tauri::command]
async fn file_history(before: Option<i64>) -> Result<xrun::history::FilePage, CommandError> {
    tauri::async_runtime::spawn_blocking(move || xrun::history::files(before))
        .await
        .map_err(CommandError::from_error)?
        .map_err(CommandError::from_error)
}

#[tauri::command]
async fn invite<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    allow: bool,
) -> Result<network::Invitation, CommandError> {
    let _guard = state.action.lock().await;
    let result = async {
        let id = xrun::config::Identity::load()?;
        Ok(serde_json::from_value(
            xrun::network::invite(&id, allow).await?,
        )?)
    }
    .await;
    record(&app, result)
}

#[tauri::command]
async fn revoke<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    device: String,
) -> Result<network::Revocation, CommandError> {
    let _guard = state.action.lock().await;
    let result = async {
        let id = xrun::config::Identity::load()?;
        Ok(serde_json::from_value(
            xrun::network::revoke(&id, &device).await?,
        )?)
    }
    .await;
    record(&app, result)
}

#[tauri::command]
fn copy_invitation<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    link: String,
) -> Result<(), CommandError> {
    let result = (|| {
        anyhow::ensure!(
            link.starts_with("xrun://") && link.len() <= 4096,
            xrun::error::ErrorCode::InvalidLink.error("expected a member invitation")
        );
        app.clipboard().write_text(link)?;
        Ok(())
    })();
    record(&app, result)
}

#[tauri::command]
async fn create_network<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    link: String,
    name: String,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    let result = async {
        xrun::network::create(link.trim(), Some(name.trim().to_string())).await?;
        xrun::daemon::init().map_err(service_start_error)?;
        platform::start().await.map_err(service_start_error)
    }
    .await;
    record(&app, result)
}

#[tauri::command]
async fn join<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    link: String,
    name: String,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    let result = async {
        xrun::network::join(link.trim(), name.trim().to_string()).await?;
        xrun::daemon::init().map_err(service_start_error)?;
        platform::start().await.map_err(service_start_error)
    }
    .await;
    record(&app, result)
}

#[tauri::command]
async fn start<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    record(&app, platform::start().await)
}
#[tauri::command]
async fn stop<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    record(&app, xrun::service::stop_daemon().await)
}
#[tauri::command]
async fn remove_service<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    record(&app, platform::remove().await)
}
#[tauri::command]
async fn autostart<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    enabled: bool,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    record(&app, platform::autostart(enabled))
}
#[tauri::command]
async fn permission<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    device: String,
    allow: bool,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    record(
        &app,
        xrun::client::set_permission(&device, allow)
            .await
            .map(|_| ()),
    )
}
#[tauri::command]
async fn all_permissions<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    allow: bool,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    let result = async {
        xrun::config::update_all_permissions(allow)?;
        xrun::control::refresh_access().await
    }
    .await;
    record(&app, result)
}
#[tauri::command]
async fn pause_access<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    state: State<'_, Desktop>,
    paused: bool,
) -> Result<(), CommandError> {
    let _guard = state.action.lock().await;
    let result = async {
        xrun::config::pause_remote_access(paused)?;
        xrun::control::refresh_access().await
    }
    .await;
    record(&app, result)
}
#[tauri::command]
fn hide_icon<R: tauri::Runtime>(app: tauri::AppHandle<R>) -> Result<(), CommandError> {
    if let Some(tray) = app.tray_by_id("xrun") {
        tray.set_visible(false).map_err(CommandError::from_error)?;
    }
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(CommandError::from_error)?;
    }
    Ok(())
}

fn menu_event<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: tauri::menu::MenuEvent) {
    match event.id.as_ref() {
        "show" => present(app),
        "hide" => {
            let _ = hide_icon(app.clone());
        }
        "quit" => app.exit(0),
        "start" | "stop" => {
            let app = app.clone();
            let start = event.id.as_ref() == "start";
            tauri::async_runtime::spawn(async move {
                let state = app.state::<Desktop>();
                let _guard = state.action.lock().await;
                let result = if start {
                    platform::start().await
                } else {
                    xrun::service::stop_daemon().await
                };
                let _ = record(&app, result);
            });
        }
        _ => {}
    }
}

fn tray<R: tauri::Runtime>(app: &tauri::App<R>) -> tauri::Result<MenuItem<R>> {
    let state = MenuItem::with_id(app, "state", "xrun · 检查状态…", false, None::<&str>)?;
    let show = MenuItem::with_id(app, "show", "打开 xrun", true, None::<&str>)?;
    let start = MenuItem::with_id(app, "start", "启动后台服务", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "停止后台服务", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, "hide", "隐藏图标", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 App（服务继续运行）", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(
        app,
        &[&state, &separator, &show, &start, &stop, &hide, &quit],
    )?;
    TrayIconBuilder::with_id("xrun")
        .icon(tauri::image::Image::from_bytes(
            if cfg!(target_os = "macos") {
                include_bytes!("../icons/tray.png")
            } else {
                include_bytes!("../icons/icon.png")
            },
        )?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip("xrun")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(menu_event)
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                } | TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) && !cfg!(target_os = "macos")
            {
                present(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(state)
}

fn commands<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .manage(Desktop::default())
        .invoke_handler(tauri::generate_handler![
            status,
            devices,
            task_history,
            task_output,
            file_history,
            invite,
            revoke,
            copy_invitation,
            create_network,
            join,
            start,
            stop,
            remove_service,
            autostart,
            permission,
            all_permissions,
            pause_access,
            hide_icon,
            settings,
            save_settings,
            choose_directory
        ])
}

fn tray_text(status: anyhow::Result<Status>) -> &'static str {
    match status {
        Ok(s) if !s.local.joined => "xrun · 尚未加入",
        Ok(s) if s.service.approval_required => "xrun · 需要系统授权",
        Ok(s) if !s.local.daemon_running => "xrun · 服务已停止",
        Ok(s) if s.local.remote_access_paused => "xrun · 远程访问已暂停",
        Ok(s) if s.local.daemon_connected == Some(true) => "xrun · 已连接",
        Ok(s) if s.local.daemon_connected.is_none() => "xrun · 旧版服务运行中",
        Ok(_) => "xrun · 连接中…",
        Err(_) => "xrun · 状态读取失败",
    }
}

fn app_builder<R: tauri::Runtime>(
    builder: tauri::Builder<R>,
    background: bool,
) -> tauri::Builder<R> {
    commands(builder)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let item = tray(app)?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let text = tray_text(local_status(&handle));
                    let _ = item.set_text(text);
                    if let Some(icon) = handle.tray_by_id("xrun") {
                        let _ = icon.set_tooltip(Some(text));
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            });
            if !background && !xrun::client::local_status()?.joined {
                present(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
}

fn run_event<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: tauri::RunEvent) {
    #[cfg(target_os = "macos")]
    if let tauri::RunEvent::Reopen { .. } = event {
        present(app);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, event);
}

fn main() {
    if std::env::args().any(|arg| arg == "--self-check") {
        let result = tauri::async_runtime::block_on(platform::check_helper()).and_then(|()| {
            let status = xrun::client::local_status()?;
            println!("{}", serde_json::to_string(&status)?);
            Ok(())
        });
        if let Err(e) = result {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
        return;
    }
    #[cfg(windows)]
    if let Some(operation) = std::env::args()
        .find(|arg| matches!(arg.as_str(), "--prepare-update" | "--prepare-uninstall"))
    {
        let result = tauri::async_runtime::block_on(async {
            if operation == "--prepare-uninstall" {
                platform::prepare_uninstall().await
            } else {
                xrun::service::stop_daemon().await
            }
        });
        if let Err(e) = result {
            println!("{e:#}");
            std::process::exit(1);
        }
        return;
    }
    let background = std::env::args().any(|arg| arg == "--background");
    app_builder(
        tauri::Builder::default()
            .plugin(tauri_plugin_single_instance::init(|app, _, _| present(app))),
        background,
    )
    .build(tauri::generate_context!())
    .expect("failed to initialize xrun desktop")
    .run(run_event);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use anyhow::Result;
    use serde_json::{Value, json};
    use std::{
        path::Path,
        process::Command,
        time::{Duration, Instant},
    };
    use tauri::test::{MockRuntime, mock_builder, mock_context, noop_assets};
    use xrun::{
        config::{self, DaemonConfig, Identity, NetworkIdentity, ServerConfig},
        membership::{Manager as NetworkManager, RosterCache},
    };

    #[cfg(target_os = "macos")]
    #[test]
    fn start_initializes_and_stops_a_development_helper_without_service_registration() -> Result<()>
    {
        isolated(
            "start_initializes_and_stops_a_development_helper_without_service_registration",
            || {
                let root = config::home_dir()?;
                seed(&relay_config(&root)?)?;
                let source = root.join("helper.rs");
                std::fs::write(&source, include_str!("../tests/fixtures/dev-daemon.rs"))?;
                let helper = std::env::current_exe()?.with_file_name("xrun");
                let build = Command::new("rustc")
                    .arg(source)
                    .arg("-o")
                    .arg(&helper)
                    .env("XRUN_FIXTURE_VERSION", xrun::protocol::VERSION)
                    .output()?;
                anyhow::ensure!(
                    build.status.success(),
                    "{}",
                    String::from_utf8_lossy(&build.stderr)
                );
                let (_app, window) = build_app();
                assert_eq!(invoke(&window, "start", json!({})), Ok(Value::Null));
                let dir = config::device_dir()?;
                assert!(config::instance_running(&dir.join("daemon.lock"))?);
                assert_eq!(xrun::control::state(&dir)?.unwrap().generation, "fixture");
                assert!(
                    !root
                        .join("Library/LaunchAgents/com.xrun.daemon.plist")
                        .exists()
                );
                assert_eq!(invoke(&window, "stop", json!({})), Ok(Value::Null));
                assert!(!config::instance_running(&dir.join("daemon.lock"))?);
                assert!(Identity::load()?.network.is_some());
                Ok(())
            },
        )
    }

    // Each case gets its own process, home and executable directory. No global
    // environment mutation or installed helper/service can leak into these tests.
    fn isolated(name: &str, test: impl FnOnce() -> Result<()>) -> Result<()> {
        if std::env::var("XRUN_NATIVE_TEST").as_deref() == Ok(name) {
            return test();
        }
        let temp = tempfile::tempdir()?;
        let exe = temp.path().join(if cfg!(windows) {
            "native-test.exe"
        } else {
            "native-test"
        });
        std::fs::copy(std::env::current_exe()?, &exe)?;
        let home = temp.path().join("home with spaces");
        std::fs::create_dir(&home)?;
        let mut child = Command::new(exe)
            .args(["--exact", &format!("tests::{name}"), "--nocapture"])
            .env("XRUN_NATIVE_TEST", name)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let deadline = Instant::now() + Duration::from_secs(90);
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                child.kill()?;
                let output = child.wait_with_output()?;
                anyhow::bail!(
                    "native command test timed out: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let output = child.wait_with_output()?;
        anyhow::ensure!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    fn build_app() -> (tauri::App<MockRuntime>, tauri::WebviewWindow<MockRuntime>) {
        let app = commands(mock_builder())
            .build(mock_context(noop_assets()))
            .unwrap();
        let window = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
            .build()
            .unwrap();
        (app, window)
    }

    fn invoke(
        window: &tauri::WebviewWindow<MockRuntime>,
        command: &str,
        body: Value,
    ) -> Result<Value, Value> {
        tauri::test::get_ipc_response(
            window,
            tauri::webview::InvokeRequest {
                cmd: command.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(windows) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
        .map(|body| body.deserialize().unwrap())
    }

    fn seed(cfg: &ServerConfig) -> Result<Identity> {
        let dir = config::device_dir()?;
        let ca = std::fs::read_to_string(cfg.data_dir.join("ca.pem"))?;
        let (manager, member, key_pem, cert_pem) = NetworkManager::create(
            &dir.join("manager"),
            "manager1",
            xrun::relay::addresses(cfg)?,
            ca,
        )?;
        let roster = manager.roster()?;
        let id = Identity {
            device_id: member.device_id,
            name: member.name,
            addresses: roster.roster.relay_addresses.clone(),
            ca_pem: roster.ca_pem.clone(),
            cert_pem,
            key_pem,
            registration: xrun::protocol::Registration {
                inviter_id: None,
                allow_inviter: false,
            },
            network: Some(NetworkIdentity {
                network_id: roster.roster.network_id.clone(),
                manager_id: roster.roster.manager_id.clone(),
            }),
        };
        RosterCache::open(&dir.join("roster.db"))?.observe(&roster.roster.network_id, &roster)?;
        id.save()?;
        xrun::daemon::init()?;
        Ok(id)
    }

    fn relay_config(path: &Path) -> Result<ServerConfig> {
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let cfg = ServerConfig {
            port,
            addresses: vec![format!("127.0.0.1:{port}")],
            manual: true,
            no_detect: true,
            data_dir: path.join("relay"),
        };
        xrun::relay::deployment_link(&cfg)?;
        Ok(cfg)
    }

    #[test]
    fn settings_persist_through_native_ipc_and_rejections_leave_config_intact() -> Result<()> {
        isolated(
            "settings_persist_through_native_ipc_and_rejections_leave_config_intact",
            || {
                let root = config::home_dir()?;
                seed(&relay_config(&root)?)?;
                let mut cfg = DaemonConfig::load()?;
                cfg.env.insert("KEEP_ME".into(), "unchanged".into());
                cfg.allow_from.push(format!("dev_{}", "1".repeat(32)));
                cfg.save()?;
                let (app, window) = build_app();
                let execution =
                    json!({"default_cwd":root,"max_concurrent_jobs":7,"path":"custom PATH"});
                assert_eq!(
                    invoke(&window, "save_settings", json!({"execution":execution})),
                    Ok(Value::Null)
                );
                assert_eq!(
                    invoke(&window, "settings", json!({})).unwrap()["execution"],
                    execution
                );
                let saved = DaemonConfig::load()?;
                assert_eq!(saved.env["KEEP_ME"], "unchanged");
                assert_eq!(saved.allow_from, cfg.allow_from);
                let bytes = std::fs::read(config::device_dir()?.join("daemon.toml"))?;
                for invalid in [
                    json!({"default_cwd":root.join("missing"),"max_concurrent_jobs":7,"path":null}),
                    json!({"default_cwd":root,"max_concurrent_jobs":0,"path":null}),
                    json!({"default_cwd":root,"max_concurrent_jobs":7,"path":"bad\u{0000}path"}),
                    json!({"default_cwd":root,"max_concurrent_jobs":7,"path":null,"allow_all":true}),
                ] {
                    assert!(
                        invoke(&window, "save_settings", json!({"execution":invalid})).is_err()
                    );
                    assert_eq!(
                        std::fs::read(config::device_dir()?.join("daemon.toml"))?,
                        bytes
                    );
                }
                assert!(app.state::<Desktop>().error.lock().unwrap().is_some());
                let defaults = json!({"default_cwd":null,"max_concurrent_jobs":4,"path":null});
                assert_eq!(
                    invoke(&window, "save_settings", json!({"execution":defaults})),
                    Ok(Value::Null)
                );
                assert!(app.state::<Desktop>().error.lock().unwrap().is_none());
                assert!(!DaemonConfig::load()?.env.contains_key("PATH"));
                drop(window);
                drop(app);
                let (_reopened, window) = build_app();
                assert_eq!(
                    invoke(&window, "settings", json!({})).unwrap()["execution"],
                    defaults
                );
                Ok(())
            },
        )
    }

    #[test]
    fn permissions_and_pause_persist_through_native_ipc() -> Result<()> {
        isolated("permissions_and_pause_persist_through_native_ipc", || {
            seed(&relay_config(&config::home_dir()?)?)?;
            let (_app, window) = build_app();
            let peer = format!("dev_{}", "a".repeat(32));
            assert_eq!(
                invoke(&window, "permission", json!({"device":peer,"allow":true})),
                Ok(Value::Null)
            );
            assert_eq!(DaemonConfig::load()?.allow_from, [peer.as_str()]);
            assert!(DaemonConfig::load()?.check_access(&peer).is_ok());
            assert_eq!(
                invoke(&window, "permission", json!({"device":peer,"allow":false})),
                Ok(Value::Null)
            );
            assert_eq!(
                invoke(&window, "all_permissions", json!({"allow":true})),
                Ok(Value::Null)
            );
            let cfg = DaemonConfig::load()?;
            assert!(cfg.allow_all && cfg.allow_from.is_empty());
            assert_eq!(cfg.deny_from, [peer.as_str()]);
            assert!(cfg.check_access(&peer).is_err());
            assert!(cfg.check_access("another-member").is_ok());
            for paused in [true, false] {
                assert_eq!(
                    invoke(&window, "pause_access", json!({"paused":paused})),
                    Ok(Value::Null)
                );
                let cfg = DaemonConfig::load()?;
                assert_eq!(cfg.remote_access_paused, paused);
                assert_eq!(cfg.check_access("another-member").is_err(), paused);
                assert!(cfg.allow_all);
                assert_eq!(cfg.deny_from, [peer.as_str()]);
                let status = invoke(&window, "status", json!({})).unwrap();
                assert_eq!(status["local"]["remote_access_paused"], paused);
                assert_eq!(status["deny_from"], json!([peer]));
                assert_eq!(status["network"]["is_manager"], true);
            }
            let bytes = std::fs::read(config::device_dir()?.join("daemon.toml"))?;
            assert!(
                invoke(
                    &window,
                    "permission",
                    json!({"device":"dev_invalid","allow":true})
                )
                .unwrap_err()["code"]
                    == "INVALID_DEVICE_ID"
            );
            assert_eq!(
                std::fs::read(config::device_dir()?.join("daemon.toml"))?,
                bytes
            );
            Ok(())
        })
    }

    struct Relay(tauri::async_runtime::JoinHandle<Result<()>>);
    impl Drop for Relay {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    fn start_relay(cfg: &ServerConfig) -> Result<Relay> {
        let task = tauri::async_runtime::spawn(xrun::relay::run(cfg.clone()));
        let relay = Relay(task);
        tauri::async_runtime::block_on(async {
            let ca = std::fs::read_to_string(cfg.data_dir.join("ca.pem"))?;
            let client = xrun::crypto::http_client(&ca, None)?;
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if client
                        .get(format!("https://127.0.0.1:{}/", cfg.port))
                        .timeout(Duration::from_millis(200))
                        .send()
                        .await
                        .is_ok()
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await?;
            Ok::<_, anyhow::Error>(())
        })?;
        Ok(relay)
    }

    fn retry_existing_daemon(window: &tauri::WebviewWindow<MockRuntime>) -> Result<()> {
        let identity = std::fs::read(config::device_dir()?.join("identity.toml"))?;
        let source = config::home_dir()?.join("helper.rs");
        std::fs::write(
            &source,
            format!(
                "fn main() {{ println!(\"xrun {}\"); }}",
                xrun::protocol::VERSION
            ),
        )?;
        let helper = std::env::current_exe()?.with_file_name(if cfg!(windows) {
            "xrun.exe"
        } else {
            "xrun"
        });
        let compiled = Command::new("rustc")
            .arg(source)
            .arg("-o")
            .arg(helper)
            .output()?;
        anyhow::ensure!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        // Model a daemon that the user started externally after registration failed.
        // The real Start handler must reuse it without re-registering the identity.
        let _running = xrun::daemon::instance_lock()?;
        assert_eq!(invoke(window, "start", json!({})), Ok(Value::Null));
        assert_eq!(
            std::fs::read(config::device_dir()?.join("identity.toml"))?,
            identity
        );
        Ok(())
    }

    #[test]
    fn creation_failure_keeps_identity_and_retry_only_starts_the_service() -> Result<()> {
        isolated(
            "creation_failure_keeps_identity_and_retry_only_starts_the_service",
            || {
                let cfg = relay_config(&config::home_dir()?)?;
                let _relay = start_relay(&cfg)?;
                let (app, window) = build_app();
                let error = invoke(
                    &window,
                    "create_network",
                    json!({"link":"invalid","name":"local1"}),
                )
                .unwrap_err();
                assert!(!(error["code"] == "SERVICE_START_FAILED"));
                assert!(Identity::load().is_err());
                let link = xrun::relay::deployment_link(&cfg)?;
                let error = invoke(
                    &window,
                    "create_network",
                    json!({"link":format!("  {link}  "),"name":" local1 "}),
                )
                .unwrap_err();
                assert!(
                    error["code"] == "SERVICE_START_FAILED"
                        && error["message"]
                            .as_str()
                            .unwrap()
                            .contains("HELPER_NOT_FOUND:")
                );
                assert_eq!(Identity::load()?.name, "local1");
                assert!(config::device_dir()?.join("daemon.initialized").exists());
                assert!(app.state::<Desktop>().error.lock().unwrap().is_some());
                let status = invoke(&window, "status", json!({})).unwrap();
                assert_eq!(status["local"]["joined"], true);
                assert!(status["error"]["code"] == "SERVICE_START_FAILED");
                retry_existing_daemon(&window)?;
                assert!(app.state::<Desktop>().error.lock().unwrap().is_none());
                assert!(invoke(&window, "status", json!({})).unwrap()["error"].is_null());
                Ok(())
            },
        )
    }

    #[test]
    #[ignore = "subprocess fixture for the native join test"]
    fn pairing_manager_fixture() -> Result<()> {
        let cfg: ServerConfig =
            config::read(Path::new(&std::env::var("XRUN_NATIVE_MANAGER_CONFIG")?))?;
        let id = seed(&cfg)?;
        tauri::async_runtime::block_on(async {
            let invitation = xrun::network::invite(&id, false).await?;
            config::atomic_private_write(
                &config::home_dir()?.join("invitation.json"),
                &serde_json::to_vec(&invitation)?,
            )?;
            xrun::daemon::run().await
        })
    }

    #[test]
    fn join_failure_keeps_membership_and_retry_does_not_reuse_the_invitation() -> Result<()> {
        isolated(
            "join_failure_keeps_membership_and_retry_does_not_reuse_the_invitation",
            || {
                let root = config::home_dir()?;
                let cfg = relay_config(&root)?;
                let _relay = start_relay(&cfg)?;
                let manager_home = root.join("manager-home");
                std::fs::create_dir(&manager_home)?;
                let cfg_path = root.join("relay.toml");
                config::write(&cfg_path, &cfg)?;
                let child = Command::new(std::env::current_exe()?)
                    .args([
                        "--exact",
                        "tests::pairing_manager_fixture",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("HOME", &manager_home)
                    .env("USERPROFILE", &manager_home)
                    .env("XRUN_NATIVE_MANAGER_CONFIG", cfg_path)
                    .spawn()?;
                struct Cleanup(std::process::Child);
                impl Drop for Cleanup {
                    fn drop(&mut self) {
                        let _ = self.0.kill();
                        let _ = self.0.wait();
                    }
                }
                let mut manager = Cleanup(child);
                let deadline = Instant::now() + Duration::from_secs(10);
                while !xrun::control::state(&manager_home.join(".xrun"))?
                    .is_some_and(|s| s.connected)
                {
                    anyhow::ensure!(manager.0.try_wait()?.is_none(), "pairing manager exited");
                    anyhow::ensure!(Instant::now() < deadline, "pairing manager did not connect");
                    std::thread::sleep(Duration::from_millis(20));
                }
                let invitation: Value =
                    serde_json::from_slice(&std::fs::read(manager_home.join("invitation.json"))?)?;
                let (_app, window) = build_app();
                let error = invoke(
                    &window,
                    "join",
                    json!({"link":invitation["link"],"name":"member1"}),
                )
                .unwrap_err();
                assert!(
                    error["code"] == "SERVICE_START_FAILED"
                        && error["message"]
                            .as_str()
                            .unwrap()
                            .contains("HELPER_NOT_FOUND:")
                );
                let id = Identity::load()?;
                assert_eq!(id.name, "member1");
                assert_ne!(id.device_id, id.network.unwrap().manager_id);
                assert!(!config::device_dir()?.join("pending.toml").exists());
                // The invitation is single-use; retrying service startup requires no link.
                retry_existing_daemon(&window)?;
                Ok(())
            },
        )
    }

    #[test]
    fn history_ipc_preserves_cursors_binary_logs_and_database_errors() -> Result<()> {
        isolated(
            "history_ipc_preserves_cursors_binary_logs_and_database_errors",
            || {
                use xrun::{protocol::*, store::TaskStore};
                let (_app, window) = build_app();
                let path = config::device_dir()?.join("daemon.db");
                let empty = invoke(&window, "task_history", json!({"filter":"all"})).unwrap();
                assert_eq!(empty["jobs"], json!([]));
                assert!(empty["db_id"].is_null());
                assert_eq!(
                    invoke(&window, "file_history", json!({})).unwrap()["entries"],
                    json!([])
                );
                assert!(!path.exists(), "opening history created a task database");
                let tasks = TaskStore::open(&path, true)?;
                for n in 1..=55 {
                    tasks.insert(&Job {
                        job_id: format!("{n:06}"),
                        request_id: format!("request-{n}"),
                        request_hash: "hash".into(),
                        source_device_id: "source".into(),
                        target_device_id: "target".into(),
                        db_id: tasks.db_id.clone(),
                        program: "program".into(),
                        args: vec!["argument".into()],
                        cwd: config::home_dir()?.to_string_lossy().into(),
                        state: if n == 55 {
                            JobState::Failed
                        } else {
                            JobState::Running
                        },
                        exit_code: None,
                        signal: None,
                        duration_ms: None,
                        last_seq: 0,
                        output_complete: true,
                        incomplete_reason: None,
                        error: None,
                        created_at_ms: now_ms(),
                        updated_at_ms: now_ms(),
                        leftover_possible: false,
                        process: None,
                    })?;
                    tasks.audit(json!({"source_device_id":"source","op":"pull","path":format!("file-{n}"),"size":2,"result":"ok"}))?;
                }
                let page = invoke(&window, "task_history", json!({"filter":"all"})).unwrap();
                assert_eq!(page["db_id"], tasks.db_id);
                assert_eq!(page["jobs"].as_array().unwrap().len(), 50);
                let older = invoke(
                    &window,
                    "task_history",
                    json!({"filter":"all","before":page["next_cursor"]}),
                )
                .unwrap();
                assert_eq!(older["jobs"].as_array().unwrap().len(), 5);
                assert_eq!(older["jobs"][0]["job_id"], "000005");
                assert!(older["next_cursor"].is_null());
                let failed = invoke(&window, "task_history", json!({"filter":"failed"})).unwrap();
                assert_eq!(failed["jobs"].as_array().unwrap().len(), 1);
                assert_eq!(failed["jobs"][0]["job_id"], "000055");
                assert!(
                    invoke(&window, "task_history", json!({"filter":"invalid"})).unwrap_err()["code"]
                        == "INVALID_FILTER"
                );
                for _ in 0..40 {
                    tasks.append("000001", "stdout", b"output\n")?;
                }
                tasks.append("000001", "stderr", &[0xff, 0])?;
                let output = invoke(
                    &window,
                    "task_output",
                    json!({"dbId":tasks.db_id,"job":"000001"}),
                )
                .unwrap();
                assert_eq!(output["events"].as_array().unwrap().len(), 32);
                assert_eq!(output["events"][0]["seq"], 10);
                let next = invoke(
                    &window,
                    "task_output",
                    json!({"dbId":tasks.db_id,"job":"000001","after":40}),
                )
                .unwrap();
                assert_eq!(
                    next["events"],
                    json!([{"seq":41,"stream":"stderr","data_base64":"/wA="}])
                );
                assert_eq!(next["has_more"], false);
                for (db, job, after, code) in [
                    ("previous-database", "000001", 0, "DB_RESET:"),
                    (tasks.db_id.as_str(), "missing", 0, "JOB_NOT_FOUND:"),
                    (tasks.db_id.as_str(), "000001", u64::MAX, "INVALID_CURSOR:"),
                ] {
                    let error = invoke(
                        &window,
                        "task_output",
                        json!({"dbId":db,"job":job,"after":after}),
                    )
                    .unwrap_err();
                    assert!(error["code"] == code.trim_end_matches(':'), "{error}");
                }
                let files = invoke(&window, "file_history", json!({})).unwrap();
                assert_eq!(files["entries"].as_array().unwrap().len(), 50);
                let older = invoke(
                    &window,
                    "file_history",
                    json!({"before":files["next_cursor"]}),
                )
                .unwrap();
                assert_eq!(older["entries"].as_array().unwrap().len(), 5);
                assert_eq!(older["entries"][0]["path"], "file-5");
                assert!(older["next_cursor"].is_null());
                Ok(())
            },
        )
    }

    #[test]
    fn native_membership_commands_preserve_invitation_policy_and_revoke_by_name() -> Result<()> {
        isolated(
            "native_membership_commands_preserve_invitation_policy_and_revoke_by_name",
            || {
                let root = config::home_dir()?;
                let cfg = relay_config(&root)?;
                let id = seed(&cfg)?;
                let _relay = start_relay(&cfg)?;
                let (_app, window) = build_app();
                let manager = xrun::network::manager(&id)?;
                let mut members = vec![];
                for (name, allow) in [("reader", false), ("operator", true)] {
                    let invite = invoke(&window, "invite", json!({"allow":allow})).unwrap();
                    assert_eq!(invite["allow"], allow);
                    assert_eq!(invite["expires_in"], 600);
                    let token = invite["link"].as_str().unwrap().rsplit_once('#').unwrap().1;
                    let (_, csr) = xrun::crypto::new_device_request()?;
                    let joined = manager.pair(token, name, &csr)?;
                    assert_eq!(joined.receipt.receipt.allow, allow);
                    xrun::network::observe(&id, &joined.roster)?;
                    members.push(joined.member.device_id);
                }
                let devices = invoke(&window, "devices", json!({})).unwrap();
                assert!(devices["server_error"].is_null(), "{devices}");
                assert_eq!(devices["devices"].as_array().unwrap().len(), 3);
                let error = invoke(&window, "revoke", json!({"device":"manager1"})).unwrap_err();
                assert!((error["code"] == "MANAGER_PROTECTED"), "{error}");
                let result = invoke(&window, "revoke", json!({"device":"reader"})).unwrap();
                assert_eq!(result["device_id"], members[0]);
                assert_eq!(result["revoked"], true);
                assert_eq!(result["undelivered"], json!([members[1]]));
                let roster = manager.roster()?;
                assert!(roster.member(&members[0])?.revoked);
                assert!(!roster.member(&members[1])?.revoked);
                assert!(invoke(&window, "status", json!({})).unwrap()["error"].is_null());
                Ok(())
            },
        )
    }

    #[test]
    fn native_stop_finishes_the_daemon_without_erasing_membership() -> Result<()> {
        isolated(
            "native_stop_finishes_the_daemon_without_erasing_membership",
            || {
                let cfg = relay_config(&config::home_dir()?)?;
                seed(&cfg)?;
                let _relay = start_relay(&cfg)?;
                let (_app, window) = build_app();
                let dir = config::device_dir()?;
                let identity = std::fs::read(dir.join("identity.toml"))?;
                let task = tauri::async_runtime::spawn(xrun::daemon::run());
                let deadline = Instant::now() + Duration::from_secs(10);
                while !xrun::control::state(&dir)?.is_some_and(|s| s.connected) {
                    anyhow::ensure!(Instant::now() < deadline, "daemon did not connect");
                    std::thread::sleep(Duration::from_millis(20));
                }
                assert_eq!(invoke(&window, "stop", json!({})), Ok(Value::Null));
                tauri::async_runtime::block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), task).await??
                })?;
                assert!(!config::instance_running(&dir.join("daemon.lock"))?);
                assert_eq!(invoke(&window, "stop", json!({})), Ok(Value::Null));
                assert_eq!(std::fs::read(dir.join("identity.toml"))?, identity);
                Ok(())
            },
        )
    }

    #[test]
    fn native_status_reports_invalid_links_and_recovers_membership_read_errors() -> Result<()> {
        isolated(
            "native_status_reports_invalid_links_and_recovers_membership_read_errors",
            || {
                seed(&relay_config(&config::home_dir()?)?)?;
                let (app, window) = build_app();
                assert_eq!(invoke(&window, "hide_icon", json!({})), Ok(Value::Null));
                let error = invoke(
                    &window,
                    "copy_invitation",
                    json!({"link":"https://not-an-invitation"}),
                )
                .unwrap_err();
                assert!((error["code"] == "INVALID_LINK"));
                assert!(app.state::<Desktop>().error.lock().unwrap().is_some());
                assert_eq!(
                    invoke(&window, "pause_access", json!({"paused":false})),
                    Ok(Value::Null)
                );
                let cache = config::device_dir()?.join("roster.db");
                let backup = cache.with_extension("backup");
                std::fs::rename(&cache, &backup)?;
                let broken = invoke(&window, "status", json!({})).unwrap();
                assert!(broken["network"].is_null());
                assert!(broken["error"]["code"] == "MEMBER_STATE_MISSING");
                std::fs::rename(backup, cache)?;
                let recovered = invoke(&window, "status", json!({})).unwrap();
                assert!(recovered["error"].is_null());
                assert_eq!(recovered["network"]["is_manager"], true);
                #[cfg(target_os = "macos")]
                {
                    // An unbundled dev process must not register a real login item.
                    assert!(
                        invoke(&window, "autostart", json!({"enabled":true})).unwrap_err()["code"]
                            == "APP_BUNDLE_REQUIRED"
                    );
                    assert_eq!(
                        invoke(&window, "remove_service", json!({})),
                        Ok(Value::Null)
                    );
                    assert!(Identity::load().is_ok());
                }
                Ok(())
            },
        )
    }

    #[test]
    fn tray_text_prioritizes_setup_approval_stop_and_pause_over_connectivity() -> Result<()> {
        isolated(
            "tray_text_prioritizes_setup_approval_stop_and_pause_over_connectivity",
            || {
                let (app, _window) = build_app();
                for (joined, approval, running, paused, connected, expected) in [
                    (false, true, true, true, Some(true), "xrun · 尚未加入"),
                    (true, true, false, false, Some(true), "xrun · 需要系统授权"),
                    (true, false, false, true, Some(true), "xrun · 服务已停止"),
                    (true, false, true, true, Some(true), "xrun · 远程访问已暂停"),
                    (true, false, true, false, Some(true), "xrun · 已连接"),
                    (true, false, true, false, None, "xrun · 旧版服务运行中"),
                    (true, false, true, false, Some(false), "xrun · 连接中…"),
                ] {
                    let mut status = local_status(app.handle())?;
                    status.local.joined = joined;
                    status.service.approval_required = approval;
                    status.local.daemon_running = running;
                    status.local.remote_access_paused = paused;
                    status.local.daemon_connected = connected;
                    assert_eq!(tray_text(Ok(status)), expected);
                }
                assert_eq!(
                    tray_text(Err(anyhow::anyhow!("unreadable config"))),
                    "xrun · 状态读取失败"
                );
                Ok(())
            },
        )
    }

    #[cfg(target_os = "macos")]
    #[allow(dead_code)] // Called by the native_ui target, which runs on the process main thread.
    pub(crate) mod native_ui {
        use super::*;
        use std::{os::unix::fs::PermissionsExt, process::Stdio};

        pub(crate) fn run() -> Result<()> {
            if let Ok(scenario) = std::env::var("XRUN_UI_SCENARIO") {
                if scenario.starts_with("self-check") {
                    super::super::main();
                    return Ok(());
                }
                return scenario_child(&scenario);
            }
            let temp = tempfile::tempdir()?;
            let exe = temp.path().join("ui-test");
            std::fs::copy(std::env::current_exe()?, &exe)?;
            for scenario in [
                "background",
                "foreground",
                "joined",
                "bundled",
                "self-check",
                "self-check-mismatch",
            ] {
                let home = temp.path().join(scenario);
                std::fs::create_dir(&home)?;
                let child_exe = if scenario == "bundled" {
                    let contents = temp.path().join("Test.app/Contents");
                    std::fs::create_dir_all(contents.join("MacOS"))?;
                    std::fs::write(
                        contents.join("Info.plist"),
                        r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>dev.qczone.xrun.native-tests</string><key>CFBundleExecutable</key><string>ui-test</string></dict></plist>"#,
                    )?;
                    let bundled = contents.join("MacOS/ui-test");
                    std::fs::copy(&exe, &bundled)?;
                    bundled
                } else {
                    exe.clone()
                };
                if scenario.starts_with("self-check") {
                    let helper = temp.path().join("xrun");
                    let version = if scenario == "self-check" {
                        xrun::protocol::VERSION
                    } else {
                        "wrong-version"
                    };
                    std::fs::write(&helper, format!("#!/bin/sh\nprintf 'xrun {version}\\n'\n"))?;
                    std::fs::set_permissions(helper, std::fs::Permissions::from_mode(0o700))?;
                }
                let mut child = Command::new(&child_exe)
                    .arg(if scenario.starts_with("self-check") {
                        "--self-check"
                    } else {
                        "--ui-test"
                    })
                    .env("XRUN_UI_SCENARIO", scenario)
                    .env("HOME", &home)
                    .env("USERPROFILE", &home)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()?;
                let deadline = Instant::now() + Duration::from_secs(25);
                while child.try_wait()?.is_none() {
                    if Instant::now() >= deadline {
                        child.kill()?;
                        let output = child.wait_with_output()?;
                        anyhow::bail!(
                            "UI scenario {scenario} timed out: {}",
                            String::from_utf8_lossy(&output.stderr)
                        );
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                let output = child.wait_with_output()?;
                assert_eq!(
                    output.status.code(),
                    Some(if scenario == "self-check-mismatch" {
                        1
                    } else {
                        0
                    }),
                    "{scenario}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
                if scenario == "self-check" {
                    let value: Value = serde_json::from_slice(&output.stdout)?;
                    assert_eq!(value["joined"], false);
                } else if scenario == "self-check-mismatch" {
                    assert!(
                        String::from_utf8_lossy(&output.stderr).contains("HELPER_VERSION_MISMATCH")
                    );
                }
                println!("native UI scenario {scenario}: passed");
            }
            Ok(())
        }

        fn select<R: tauri::Runtime>(app: &tauri::AppHandle<R>, id: &str) {
            menu_event(app, tauri::menu::MenuEvent { id: id.into() });
        }

        fn scenario_child(scenario: &str) -> Result<()> {
            let joined = scenario == "joined";
            let background = scenario == "background";
            if joined {
                seed(&relay_config(&config::home_dir()?)?)?;
            }
            if scenario == "bundled" {
                let status = platform::status()?;
                assert!(!status.development && !status.installed && !status.app_at_login);
            }
            let mut context = mock_context(noop_assets());
            context.config_mut().identifier = "dev.qczone.xrun.native-tests".into();
            // Production setup and callbacks, using the real Cocoa/Wry runtime. The
            // single-instance plugin is omitted so an installed xrun is never contacted.
            let app = app_builder(tauri::Builder::default(), background).build(context)?;
            tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
                .visible(false)
                .build()?;
            let code = app.run_return(move |app, event| match event {
                tauri::RunEvent::Ready => {
                    assert!(app.tray_by_id("xrun").is_some());
                    let window = app.get_webview_window("main").unwrap();
                    assert_eq!(window.is_visible().unwrap(), !background && !joined);
                    select(app, "show");
                    assert!(window.is_visible().unwrap());
                    select(app, "hide");
                    assert!(!window.is_visible().unwrap());
                    select(app, "show");
                    assert!(window.is_visible().unwrap());
                    run_event(app, tauri::RunEvent::Resumed);
                    select(app, "unrecognized-menu-id");
                    window.close().unwrap();
                }
                tauri::RunEvent::WindowEvent {
                    event: tauri::WindowEvent::CloseRequested { .. },
                    ..
                } => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        // Closing the window must hide it and retain the tray process.
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        let handle = app.clone();
                        app.run_on_main_thread(move || {
                            let window = handle.get_webview_window("main").unwrap();
                            assert!(!window.is_visible().unwrap());
                            select(&handle, "start");
                        })
                        .unwrap();
                        tokio::time::timeout(Duration::from_secs(5), async {
                            loop {
                                if app
                                    .state::<Desktop>()
                                    .error
                                    .lock()
                                    .unwrap()
                                    .as_ref()
                                    .is_some_and(|e| e.code == "HELPER_NOT_FOUND")
                                {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                        })
                        .await
                        .unwrap();
                        select(&app, "stop");
                        tokio::time::timeout(Duration::from_secs(5), async {
                            loop {
                                if app.state::<Desktop>().error.lock().unwrap().is_none() {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                        })
                        .await
                        .unwrap();
                        // Allow the production status updater to complete a second pass.
                        tokio::time::sleep(Duration::from_millis(2100)).await;
                        select(&app, "quit");
                    });
                }
                _ => {}
            });
            assert_eq!(code, 0);
            assert!(!config::instance_running(
                &config::device_dir()?.join("daemon.lock")
            )?);
            Ok(())
        }
    }
}
