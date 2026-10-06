//! Tray visibility, actions and user-visible service status.
use super::{
    app::{Desktop, Status, record},
    platform,
};
use tauri::{
    Manager,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

pub(super) fn hide<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id("xrun") {
        tray.set_visible(false)?;
    }
    if let Some(window) = app.get_webview_window("main") {
        window.hide()?;
    }
    Ok(())
}
pub(super) fn present<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(tray) = app.tray_by_id("xrun") {
        let _ = tray.set_visible(true);
    }
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

pub(super) fn menu_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event: tauri::menu::MenuEvent,
) {
    match event.id.as_ref() {
        "show" => present(app),
        "hide" => {
            let _ = hide(app);
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
                    xrun::client::services::stop_daemon().await
                };
                let _ = record(&app, result);
            });
        }
        _ => {}
    }
}

pub(super) fn build<R: tauri::Runtime>(app: &tauri::App<R>) -> tauri::Result<MenuItem<R>> {
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

pub(super) fn text(status: anyhow::Result<Status>) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::super::{
        app::local_status,
        commands::tests::{build_app, isolated},
    };
    use super::*;
    use anyhow::Result;
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
                    assert_eq!(text(Ok(status)), expected);
                }
                assert_eq!(
                    text(Err(anyhow::anyhow!("unreadable config"))),
                    "xrun · 状态读取失败"
                );
                Ok(())
            },
        )
    }
}
