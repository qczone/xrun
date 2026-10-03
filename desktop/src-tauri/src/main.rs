#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod network;
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
    error: Mutex<Option<String>>,
}

#[derive(Serialize)]
struct Status {
    local: xrun::client::LocalStatus,
    network: Option<network::NetworkStatus>,
    service: platform::ServiceStatus,
    allow_from: Vec<String>,
    deny_from: Vec<String>,
    error: Option<String>,
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
fn settings() -> Result<Settings, String> {
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
    result.map_err(|e| e.to_string())
}

#[tauri::command]
async fn save_settings(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    execution: ExecutionSettings,
) -> Result<(), String> {
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
async fn choose_directory(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .set_title("选择默认工作目录")
        .pick_folder(move |path| {
            let _ = tx.send(path);
        });
    let path = rx.await.map_err(|e| e.to_string())?;
    path.map(|p| {
        p.into_path()
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| e.to_string())
    })
    .transpose()
}

fn local_status(app: &tauri::AppHandle) -> anyhow::Result<Status> {
    let mut local = xrun::client::local_status()?;
    let service = platform::status()?;
    local.daemon_installed = service.installed || service.legacy_installed;
    let policy = xrun::config::DaemonConfig::load()?;
    let mut error = app.state::<Desktop>().error.lock().unwrap().clone();
    let network = if local.joined {
        match network::local_status() {
            Ok(value) => Some(value),
            Err(e) => {
                error = Some(e.to_string());
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

fn present(app: &tauri::AppHandle) {
    if let Some(tray) = app.tray_by_id("xrun") {
        let _ = tray.set_visible(true);
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn record<T>(app: &tauri::AppHandle, result: anyhow::Result<T>) -> Result<T, String> {
    let result = result.map_err(|e| e.to_string());
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

#[tauri::command]
fn status(app: tauri::AppHandle) -> Result<Status, String> {
    local_status(&app).map_err(|e| e.to_string())
}

#[tauri::command]
async fn devices() -> Result<xrun::client::Status, String> {
    xrun::client::status().await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn task_history(
    before: Option<i64>,
    filter: String,
) -> Result<xrun::history::TaskPage, String> {
    tauri::async_runtime::spawn_blocking(move || xrun::history::tasks(before, &filter))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn task_output(
    db_id: String,
    job: String,
    after: Option<u64>,
) -> Result<xrun::history::TaskOutput, String> {
    tauri::async_runtime::spawn_blocking(move || xrun::history::output(&db_id, &job, after))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn file_history(before: Option<i64>) -> Result<xrun::history::FilePage, String> {
    tauri::async_runtime::spawn_blocking(move || xrun::history::files(before))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn invite(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    allow: bool,
) -> Result<network::Invitation, String> {
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
fn copy_invitation(app: tauri::AppHandle, link: String) -> Result<(), String> {
    let result = (|| {
        anyhow::ensure!(
            link.starts_with("xrun://") && link.len() <= 4096,
            "INVALID_LINK: expected a member invitation"
        );
        app.clipboard().write_text(link)?;
        Ok(())
    })();
    record(&app, result)
}

#[tauri::command]
async fn create_network(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    link: String,
    name: String,
) -> Result<(), String> {
    let _guard = state.action.lock().await;
    let result = async {
        xrun::network::create(link.trim(), Some(name.trim().to_string())).await?;
        xrun::daemon::init()?;
        platform::start().await
    }
    .await;
    record(&app, result)
}

#[tauri::command]
async fn join(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    link: String,
    name: String,
) -> Result<(), String> {
    let _guard = state.action.lock().await;
    let result = async {
        xrun::client::join(link.trim(), Some(name.trim().to_string())).await?;
        platform::start().await
    }
    .await;
    record(&app, result)
}

#[tauri::command]
async fn start(app: tauri::AppHandle, state: State<'_, Desktop>) -> Result<(), String> {
    let _guard = state.action.lock().await;
    record(&app, platform::start().await)
}
#[tauri::command]
async fn stop(app: tauri::AppHandle, state: State<'_, Desktop>) -> Result<(), String> {
    let _guard = state.action.lock().await;
    record(&app, xrun::service::stop_daemon().await)
}
#[tauri::command]
async fn remove_service(app: tauri::AppHandle, state: State<'_, Desktop>) -> Result<(), String> {
    let _guard = state.action.lock().await;
    record(&app, platform::remove().await)
}
#[tauri::command]
async fn autostart(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    enabled: bool,
) -> Result<(), String> {
    let _guard = state.action.lock().await;
    record(&app, platform::autostart(enabled))
}
#[tauri::command]
async fn permission(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    device: String,
    allow: bool,
) -> Result<(), String> {
    let _guard = state.action.lock().await;
    record(
        &app,
        xrun::client::set_permission(&device, allow)
            .await
            .map(|_| ()),
    )
}
#[tauri::command]
async fn all_permissions(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    allow: bool,
) -> Result<(), String> {
    let _guard = state.action.lock().await;
    record(&app, xrun::config::update_all_permissions(allow))
}
#[tauri::command]
async fn pause_access(
    app: tauri::AppHandle,
    state: State<'_, Desktop>,
    paused: bool,
) -> Result<(), String> {
    let _guard = state.action.lock().await;
    record(&app, xrun::config::pause_remote_access(paused))
}
#[tauri::command]
fn hide_icon(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(tray) = app.tray_by_id("xrun") {
        tray.set_visible(false).map_err(|e| e.to_string())?;
    }
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn tray(app: &tauri::App) -> tauri::Result<MenuItem<tauri::Wry>> {
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
        .on_menu_event(|app, event| match event.id.as_ref() {
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
        })
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
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| present(app)))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(Desktop::default())
        .invoke_handler(tauri::generate_handler![
            status,
            devices,
            task_history,
            task_output,
            file_history,
            invite,
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
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let item = tray(app)?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let text = match local_status(&handle) {
                        Ok(s) if !s.local.joined => "xrun · 尚未加入",
                        Ok(s) if s.service.approval_required => "xrun · 需要系统授权",
                        Ok(s) if !s.local.daemon_running => "xrun · 服务已停止",
                        Ok(s) if s.local.remote_access_paused => "xrun · 远程访问已暂停",
                        Ok(s) if s.local.daemon_connected == Some(true) => "xrun · 已连接",
                        Ok(s) if s.local.daemon_connected.is_none() => "xrun · 旧版服务运行中",
                        Ok(_) => "xrun · 连接中…",
                        Err(_) => "xrun · 状态读取失败",
                    };
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
        .build(tauri::generate_context!())
        .expect("failed to initialize xrun desktop")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                present(app);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}
