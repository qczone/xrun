//! Tray visibility, actions and user-visible service status.
use super::{
    app::{Desktop, DesktopPage, navigate, record},
    bridge,
    language::{Language, LanguageState},
    lightweight, platform,
};
use serde_json::json;
use std::sync::Mutex;
use tauri::{
    Manager,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
};

struct TrayMenu<R: tauri::Runtime> {
    menu: Menu<R>,
    action: Mutex<PrimaryAction>,
}

fn labels(language: Language) -> [(&'static str, &'static str); 5] {
    [
        ("show", language.text("Open xrun", "打开 xrun")),
        ("activity", language.text("Activity", "活动")),
        ("settings", language.text("Settings", "设置")),
        ("lightweight", language.text("Lightweight mode", "轻量模式")),
        ("quit", language.text("Quit xrun", "退出 xrun")),
    ]
}

pub(super) fn refresh<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    let language = app.state::<LanguageState>().settings().language;
    update_menu(app, language, menu_view(language, current_status()))
}

pub(super) fn refresh_lightweight<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
) -> tauri::Result<()> {
    if let Some(menu) = app.try_state::<TrayMenu<R>>()
        && let Some(item) = menu
            .menu
            .get("lightweight")
            .and_then(|item| item.as_menuitem().cloned())
    {
        item.set_enabled(app.get_webview_window("main").is_some())?;
    }
    Ok(())
}

fn update_menu<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    language: Language,
    view: MenuView,
) -> tauri::Result<()> {
    if let Some(menu) = app.try_state::<TrayMenu<R>>() {
        for (id, label) in labels(language).into_iter().chain([
            ("device", view.device.as_str()),
            ("state", view.status),
            ("service", view.action.label(language)),
        ]) {
            if let Some(item) = menu
                .menu
                .get(id)
                .and_then(|item| item.as_menuitem().cloned())
            {
                item.set_text(label)?;
                if id == "service" {
                    item.set_enabled(
                        !view.action.changes_service()
                            || app.state::<Desktop>().action.try_lock().is_ok(),
                    )?;
                } else if id == "lightweight" {
                    item.set_enabled(app.get_webview_window("main").is_some())?;
                }
            }
        }
        *menu.action.lock().unwrap() = view.action;
        if let Some(icon) = app.tray_by_id("xrun") {
            icon.set_tooltip(Some(format!("{} · {}", view.device, view.status)))?;
        }
    }
    Ok(())
}
pub(super) fn hide<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<()> {
    if let Some(tray) = app.tray_by_id("xrun") {
        tray.set_visible(false)?;
    }
    lightweight::hide(app, false)
}
pub(super) fn present<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(tray) = app.tray_by_id("xrun") {
        let _ = tray.set_visible(true);
    }
    lightweight::present(app);
}

pub(super) fn menu_event<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    event: tauri::menu::MenuEvent,
) {
    match event.id.as_ref() {
        "show" => present(app),
        "activity" => navigate(app, DesktopPage::Activity),
        "settings" => navigate(app, DesktopPage::Settings),
        "lightweight" => {
            let _ = record(app, lightweight::hide(app, true).map_err(Into::into));
        }
        "quit" => app.exit(0),
        "service" => {
            // Execute the action shown in the menu. An older Pause label must
            // never turn into Resume because a background refresh raced the click.
            let action = app
                .try_state::<TrayMenu<R>>()
                .map(|menu| *menu.action.lock().unwrap())
                .unwrap_or_else(|| menu_view(Language::En, current_status()).action);
            match action {
                PrimaryAction::Join => {
                    navigate(app, DesktopPage::Devices);
                    return;
                }
                PrimaryAction::Inspect => {
                    navigate(app, DesktopPage::Settings);
                    return;
                }
                _ => {}
            }
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<Desktop>();
                let Ok(guard) = state.action.try_lock() else {
                    return;
                };
                let _ = refresh(&app);
                let result = match action {
                    PrimaryAction::Start => {
                        match bridge::get::<bridge::LocalStatus>("local_status", json!({})).await {
                            Ok(status) if !status.joined => {
                                navigate(&app, DesktopPage::Devices);
                                Ok(())
                            }
                            Ok(_) => platform::start().await,
                            Err(error) => Err(error),
                        }
                    }
                    PrimaryAction::Pause | PrimaryAction::Resume => {
                        let paused = action == PrimaryAction::Pause;
                        let params = json!({ "paused": paused });
                        bridge::call("pause_access", params).await.map(drop)
                    }
                    PrimaryAction::Join | PrimaryAction::Inspect => unreachable!(),
                };
                let _ = record(&app, result);
                drop(guard);
                let _ = refresh(&app);
            });
        }
        _ => {}
    }
}

pub(super) fn build<R: tauri::Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    let language = app.state::<LanguageState>().settings().language;
    let view = menu_view(language, current_status());
    let device = MenuItem::with_id(app, "device", &view.device, false, None::<&str>)?;
    let state = MenuItem::with_id(app, "state", view.status, false, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&device, &state, &separator])?;
    for (id, label) in labels(language) {
        if id == "quit" {
            menu.append(&PredefinedMenuItem::separator(app)?)?;
            menu.append(&MenuItem::with_id(
                app,
                "service",
                view.action.label(language),
                true,
                None::<&str>,
            )?)?;
            menu.append(&PredefinedMenuItem::separator(app)?)?;
        }
        let enabled = id != "lightweight" || app.get_webview_window("main").is_some();
        menu.append(&MenuItem::with_id(app, id, label, enabled, None::<&str>)?)?;
    }
    app.manage(TrayMenu {
        menu: menu.clone(),
        action: Mutex::new(view.action),
    });
    TrayIconBuilder::with_id("xrun")
        .icon(tauri::image::Image::from_bytes(
            if cfg!(target_os = "macos") {
                include_bytes!("../icons/tray.png")
            } else {
                include_bytes!("../icons/icon.png")
            },
        )?)
        .icon_as_template(cfg!(target_os = "macos"))
        .tooltip(format!("{} · {}", view.device, view.status))
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(menu_event)
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::DoubleClick {
                    button: MouseButton::Left,
                    ..
                }
            ) && !cfg!(target_os = "macos")
            {
                present(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

struct TrayStatus {
    local: bridge::LocalStatus,
    service: platform::ServiceStatus,
}

fn current_status() -> anyhow::Result<TrayStatus> {
    // The tray needs only local service state. Do not reopen and verify the full
    // signed member roster on every background status tick.
    Ok(TrayStatus {
        local: bridge::get_blocking("local_status", json!({}))?,
        service: platform::status()?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimaryAction {
    Join,
    Start,
    Pause,
    Resume,
    Inspect,
}

impl PrimaryAction {
    fn label(self, language: Language) -> &'static str {
        match self {
            Self::Join => language.text("Join a network…", "加入网络…"),
            Self::Start => language.text("Start service", "启动服务"),
            Self::Pause => language.text("Pause access", "暂停访问"),
            Self::Resume => language.text("Resume access", "恢复访问"),
            Self::Inspect => language.text("View details…", "查看详情…"),
        }
    }

    fn changes_service(self) -> bool {
        matches!(self, Self::Start | Self::Pause | Self::Resume)
    }
}

struct MenuView {
    device: String,
    status: &'static str,
    action: PrimaryAction,
}

fn menu_view(language: Language, status: anyhow::Result<TrayStatus>) -> MenuView {
    let Ok(s) = status else {
        return MenuView {
            device: "xrun".into(),
            status: language.text("Status unavailable", "状态读取失败"),
            action: PrimaryAction::Inspect,
        };
    };
    let state = if !s.local.joined {
        language.text("Not joined", "未加入网络")
    } else if s.service.approval_required {
        language.text("Approval needed", "待系统授权")
    } else if !s.local.daemon_running {
        language.text("Service stopped", "服务已停止")
    } else if s.local.remote_access_paused {
        language.text("Access paused", "访问已暂停")
    } else if s.local.daemon_connected == Some(true) {
        language.text("Connected", "已连接")
    } else if s.local.daemon_connected.is_none() {
        language.text("Starting…", "启动中…")
    } else {
        language.text("Connecting…", "连接中…")
    };
    let name = s
        .local
        .name
        .filter(|name| !name.is_empty())
        .unwrap_or("xrun".into());
    // Keep a long user-supplied name from widening the native menu.
    let mut characters = name.chars();
    let mut device: String = characters.by_ref().take(16).collect();
    if characters.next().is_some() {
        device.push('…');
    }
    MenuView {
        device,
        status: state,
        action: if !s.local.joined {
            PrimaryAction::Join
        } else if !s.local.daemon_running {
            PrimaryAction::Start
        } else if s.local.remote_access_paused {
            PrimaryAction::Resume
        } else {
            PrimaryAction::Pause
        },
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
    fn tray_join_without_identity_opens_setup_without_initializing_a_service() -> Result<()> {
        isolated(
            "tray_join_without_identity_opens_setup_without_initializing_a_service",
            || {
                use tauri::Listener;
                let (app, _window) = build_app();
                let directory = xrun::client::services::data_dir()?;
                assert!(!local_status(app.handle())?.local.joined);
                let (tx, rx) = std::sync::mpsc::channel();
                app.listen("xrun-window-visible", move |event| {
                    tx.send(event.payload().to_owned()).unwrap();
                });
                menu_event(
                    app.handle(),
                    tauri::menu::MenuEvent {
                        id: "service".into(),
                    },
                );
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
    pub(crate) fn assert_lightweight_available<R: tauri::Runtime>(
        app: &tauri::AppHandle<R>,
        enabled: bool,
    ) {
        let menu = app.state::<TrayMenu<R>>();
        let item = menu.menu.get("lightweight").unwrap();
        assert_eq!(item.as_menuitem().unwrap().is_enabled().unwrap(), enabled);
    }

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
            refresh(app)?;
            let menu = app.state::<TrayMenu<R>>();
            let show = menu.menu.get("show").unwrap();
            assert_eq!(show.as_menuitem().unwrap().text()?, expected);
            for (id, label) in labels(state.settings().language) {
                let item = menu.menu.get(id).unwrap();
                assert_eq!(item.as_menuitem().unwrap().text()?, label);
            }
        }
        state.save(original)?;
        refresh(app)?;
        let menu = app.state::<TrayMenu<R>>();
        let ids: Vec<String> = menu
            .menu
            .items()?
            .into_iter()
            .filter_map(|item| item.as_menuitem().map(|item| item.id().as_ref().to_owned()))
            .collect();
        assert_eq!(
            ids,
            [
                "device",
                "state",
                "show",
                "activity",
                "settings",
                "lightweight",
                "service",
                "quit"
            ]
        );
        for id in ["device", "state"] {
            assert!(
                !menu
                    .menu
                    .get(id)
                    .unwrap()
                    .as_menuitem()
                    .unwrap()
                    .is_enabled()?
            );
        }
        for (action, expected) in [
            (PrimaryAction::Start, "启动服务"),
            (PrimaryAction::Pause, "暂停访问"),
            (PrimaryAction::Resume, "恢复访问"),
        ] {
            update_menu(
                app,
                Language::Zh,
                MenuView {
                    device: "mac-pro".into(),
                    status: "测试状态",
                    action,
                },
            )?;
            let item = menu.menu.get("service").unwrap();
            let item = item.as_menuitem().unwrap();
            assert_eq!(item.text()?, expected);
            assert!(item.is_enabled()?);
            let desktop = app.state::<Desktop>();
            let guard = tauri::async_runtime::block_on(desktop.action.lock());
            update_menu(
                app,
                Language::Zh,
                MenuView {
                    device: "mac-pro".into(),
                    status: "测试状态",
                    action,
                },
            )?;
            assert!(!item.is_enabled()?);
            drop(guard);
        }
        refresh(app)?;
        Ok(())
    }

    #[test]
    fn tray_text_prioritizes_setup_approval_stop_and_pause_over_connectivity() -> Result<()> {
        isolated(
            "tray_text_prioritizes_setup_approval_stop_and_pause_over_connectivity",
            || {
                let (app, _window) = build_app();
                assert_eq!(
                    menu_view(Language::En, current_status()).status,
                    "Not joined"
                );
                for (joined, approval, running, paused, connected, expected, action) in [
                    (
                        false,
                        true,
                        true,
                        true,
                        Some(true),
                        "未加入网络",
                        PrimaryAction::Join,
                    ),
                    (
                        true,
                        true,
                        false,
                        false,
                        Some(true),
                        "待系统授权",
                        PrimaryAction::Start,
                    ),
                    (
                        true,
                        false,
                        false,
                        true,
                        Some(true),
                        "服务已停止",
                        PrimaryAction::Start,
                    ),
                    (
                        true,
                        false,
                        true,
                        true,
                        Some(true),
                        "访问已暂停",
                        PrimaryAction::Resume,
                    ),
                    (
                        true,
                        false,
                        true,
                        false,
                        Some(true),
                        "已连接",
                        PrimaryAction::Pause,
                    ),
                    (
                        true,
                        false,
                        true,
                        false,
                        None,
                        "启动中…",
                        PrimaryAction::Pause,
                    ),
                    (
                        true,
                        false,
                        true,
                        false,
                        Some(false),
                        "连接中…",
                        PrimaryAction::Pause,
                    ),
                ] {
                    let mut status = local_status(app.handle())?;
                    status.local.joined = joined;
                    status.service.approval_required = approval;
                    status.local.daemon_running = running;
                    status.local.remote_access_paused = paused;
                    status.local.daemon_connected = connected;
                    let view = menu_view(
                        Language::Zh,
                        Ok(TrayStatus {
                            local: status.local,
                            service: status.service,
                        }),
                    );
                    assert_eq!(view.status, expected);
                    assert!(view.status.chars().count() <= 6);
                    assert_eq!(view.action, action);
                }
                let view = menu_view(Language::Zh, Err(anyhow::anyhow!("unreadable config")));
                assert_eq!(view.status, "状态读取失败");
                assert_eq!(view.action, PrimaryAction::Inspect);
                assert_eq!(view.device, "xrun");
                let mut status = local_status(app.handle())?;
                status.local.joined = true;
                status.local.daemon_running = true;
                status.local.daemon_connected = Some(true);
                status.service.approval_required = false;
                assert_eq!(
                    menu_view(
                        Language::En,
                        Ok(TrayStatus {
                            local: status.local,
                            service: status.service
                        })
                    )
                    .status,
                    "Connected"
                );
                Ok(())
            },
        )
    }
}
