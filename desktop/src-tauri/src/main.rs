#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod platform;

use serde::Serialize;
use std::sync::Mutex;
use tauri::{
    Manager, State,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

#[derive(Default)]
struct Desktop {
    action: tokio::sync::Mutex<()>,
    error: Mutex<Option<String>>,
}

#[derive(Serialize)]
struct Status {
    local: xrun::client::LocalStatus,
    service: platform::ServiceStatus,
    allow_from: Vec<String>,
    error: Option<String>,
}

fn local_status(app: &tauri::AppHandle) -> anyhow::Result<Status> {
    let mut local = xrun::client::local_status()?;
    let service = platform::status()?;
    local.daemon_installed = service.installed || service.legacy_installed;
    Ok(Status {
        local,
        service,
        allow_from: xrun::config::DaemonConfig::load()?.allow_from,
        error: app.state::<Desktop>().error.lock().unwrap().clone(),
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

fn record(app: &tauri::AppHandle, result: anyhow::Result<()>) -> Result<(), String> {
    let error = result.err().map(|e| e.to_string());
    *app.state::<Desktop>().error.lock().unwrap() = error.clone();
    match error {
        Some(e) => {
            present(app);
            Err(e)
        }
        None => Ok(()),
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
        .manage(Desktop::default())
        .invoke_handler(tauri::generate_handler![
            status,
            devices,
            join,
            start,
            stop,
            remove_service,
            autostart,
            permission,
            hide_icon
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
