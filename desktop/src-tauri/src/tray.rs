//! Tray visibility, actions and user-visible service status.
use super::{
    app::{Desktop, record},
    language::{Language, LanguageState},
    platform,
};
use tauri::{
    Emitter, Manager,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};

struct TrayMenu<R: tauri::Runtime>(Menu<R>);

fn labels(language: Language) -> [(&'static str, &'static str); 5] {
    [
        ("show", language.text("Open xrun", "打开 xrun")),
        (
            "start",
            language.text("Start background service", "启动后台服务"),
        ),
        (
            "stop",
            language.text("Stop background service", "停止后台服务"),
        ),
        ("hide", language.text("Hide icon", "隐藏图标")),
        (
            "quit",
            language.text(
                "Quit app (service keeps running)",
                "退出 App（服务继续运行）",
            ),
        ),
    ]
}

pub(super) fn refresh_language<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    let language = app.state::<LanguageState>().settings().language;
    if let Some(menu) = app.try_state::<TrayMenu<R>>() {
        let status = current_text(language);
        for (id, label) in labels(language).into_iter().chain([("state", status)]) {
            if let Some(item) = menu.0.get(id).and_then(|item| item.as_menuitem().cloned()) {
                item.set_text(label)?;
            }
        }
        if let Some(icon) = app.tray_by_id("xrun") {
            icon.set_tooltip(Some(status))?;
        }
    }
    Ok(())
}
pub(super) fn hide<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id("xrun") {
        tray.set_visible(false)?;
    }
    if let Some(window) = app.get_webview_window("main") {
        window.hide()?;
        window.emit("xrun-window-visible", false)?;
    }
    Ok(())
}
pub(super) fn present<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(tray) = app.tray_by_id("xrun") {
        let _ = tray.set_visible(true);
    }
    if let Some(window) = app.get_webview_window("main") {
        if window.show().is_ok() && window.unminimize().is_ok() {
            let _ = window.emit("xrun-window-visible", true);
        }
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
                    match xrun::client::local_status() {
                        Ok(status) if !status.joined => {
                            present(&app);
                            Ok(())
                        }
                        Ok(_) => platform::start().await,
                        Err(error) => Err(error),
                    }
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
    let language = app.state::<LanguageState>().settings().language;
    let state = MenuItem::with_id(
        app,
        "state",
        language.text("xrun · Checking status…", "xrun · 检查状态…"),
        false,
        None::<&str>,
    )?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&state, &separator])?;
    for (id, label) in labels(language) {
        menu.append(&MenuItem::with_id(app, id, label, true, None::<&str>)?)?;
    }
    app.manage(TrayMenu(menu.clone()));
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

struct TrayStatus {
    local: xrun::client::LocalStatus,
    service: platform::ServiceStatus,
}

pub(super) fn current_text(language: Language) -> &'static str {
    // The tray needs only local service state. Do not reopen and verify the full
    // signed member roster on every background status tick.
    let status = (|| {
        Ok(TrayStatus {
            local: xrun::client::local_status()?,
            service: platform::status()?,
        })
    })();
    text(language, status)
}

fn text(language: Language, status: anyhow::Result<TrayStatus>) -> &'static str {
    match status {
        Ok(s) if !s.local.joined => language.text("xrun · Not joined", "xrun · 尚未加入"),
        Ok(s) if s.service.approval_required => {
            language.text("xrun · Approval required", "xrun · 需要系统授权")
        }
        Ok(s) if !s.local.daemon_running => {
            language.text("xrun · Service stopped", "xrun · 服务已停止")
        }
        Ok(s) if s.local.remote_access_paused => {
            language.text("xrun · Remote access paused", "xrun · 远程访问已暂停")
        }
        Ok(s) if s.local.daemon_connected == Some(true) => {
            language.text("xrun · Connected", "xrun · 已连接")
        }
        Ok(s) if s.local.daemon_connected.is_none() => {
            language.text("xrun · Older service running", "xrun · 旧版服务运行中")
        }
        Ok(_) => language.text("xrun · Connecting…", "xrun · 连接中…"),
        Err(_) => language.text("xrun · Status unavailable", "xrun · 状态读取失败"),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::{
        app::local_status,
        commands::tests::{build_app, isolated},
    };
    use super::*;
    use anyhow::Result;

    #[test]
    fn tray_start_without_identity_opens_setup_without_initializing_a_service() -> Result<()> {
        isolated(
            "tray_start_without_identity_opens_setup_without_initializing_a_service",
            || {
                use tauri::Listener;
                let (app, _window) = build_app();
                let directory = xrun::client::services::data_dir()?;
                assert!(!local_status(app.handle())?.local.joined);
                let (tx, rx) = std::sync::mpsc::channel();
                app.listen("xrun-window-visible", move |event| {
                    tx.send(event.payload().to_owned()).unwrap();
                });
                menu_event(app.handle(), tauri::menu::MenuEvent { id: "start".into() });
                assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5))?, "true");
                tauri::async_runtime::block_on(async {
                    let state = app.state::<Desktop>();
                    let _guard = state.action.lock().await;
                    assert!(state.error.lock().unwrap().is_none());
                });
                let status = local_status(app.handle())?;
                assert!(!status.local.joined && !status.local.daemon_running);
                assert!(status.error.is_none());
                for name in [
                    "identity.toml",
                    "daemon.initialized",
                    "daemon.db",
                    "daemon.service",
                ] {
                    assert!(
                        !directory.join(name).exists(),
                        "created {name} before joining"
                    );
                }
                Ok(())
            },
        )
    }

    // Run against the real menu on the process main thread in native_ui.
    #[cfg(target_os = "macos")]
    pub(crate) fn verify_language_switch<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
    ) -> Result<()> {
        use super::super::language::LanguagePreference;
        let state = app.state::<LanguageState>();
        let original = state.settings().preference;
        for (preference, expected) in [
            (LanguagePreference::En, "Open xrun"),
            (LanguagePreference::Zh, "打开 xrun"),
        ] {
            state.save(preference)?;
            refresh_language(app)?;
            let menu = app.state::<TrayMenu<R>>();
            let show = menu.0.get("show").unwrap();
            assert_eq!(show.as_menuitem().unwrap().text()?, expected);
        }
        state.save(original)?;
        refresh_language(app)?;
        Ok(())
    }

    #[test]
    fn tray_text_prioritizes_setup_approval_stop_and_pause_over_connectivity() -> Result<()> {
        isolated(
            "tray_text_prioritizes_setup_approval_stop_and_pause_over_connectivity",
            || {
                let (app, _window) = build_app();
                assert_eq!(current_text(Language::En), "xrun · Not joined");
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
                    assert_eq!(
                        text(
                            Language::Zh,
                            Ok(TrayStatus {
                                local: status.local,
                                service: status.service
                            })
                        ),
                        expected
                    );
                }
                assert_eq!(
                    text(Language::Zh, Err(anyhow::anyhow!("unreadable config"))),
                    "xrun · 状态读取失败"
                );
                let mut status = local_status(app.handle())?;
                status.local.joined = true;
                status.local.daemon_running = true;
                status.local.daemon_connected = Some(true);
                status.service.approval_required = false;
                assert_eq!(
                    text(
                        Language::En,
                        Ok(TrayStatus {
                            local: status.local,
                            service: status.service
                        })
                    ),
                    "xrun · Connected"
                );
                Ok(())
            },
        )
    }
}
