//! Native state, feedback and application lifecycle assembly.
use super::{commands, error::CommandError, language::LanguageState, platform, tray};
use serde::Serialize;
use std::sync::Mutex;
use tauri::{Emitter, Manager};
#[derive(Default)]
pub(super) struct Desktop {
    pub(super) action: tokio::sync::Mutex<()>,
    pub(super) error: Mutex<Option<CommandError>>,
}

#[derive(Serialize)]
pub(super) struct Status {
    pub(super) local: xrun::client::LocalStatus,
    pub(super) network: Option<xrun::client::NetworkStatus>,
    pub(super) service: platform::ServiceStatus,
    pub(super) allow_from: Vec<String>,
    pub(super) deny_from: Vec<String>,
    pub(super) error: Option<CommandError>,
}

pub(super) fn local_status<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> anyhow::Result<Status> {
    let snapshot = xrun::client::local_snapshot()?;
    let mut local = snapshot.local;
    let service = platform::status()?;
    local.daemon_installed = service.installed || service.legacy_installed;
    let error = match snapshot.network_error {
        Some(xrun::protocol::Data::Error { code, message }) => Some(CommandError { code, message }),
        _ => app.state::<Desktop>().error.lock().unwrap().clone(),
    };
    Ok(Status {
        local,
        network: snapshot.network,
        service,
        allow_from: snapshot.allow_from,
        deny_from: snapshot.deny_from,
        error,
    })
}

pub(super) fn record<T, R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    result: anyhow::Result<T>,
) -> Result<T, CommandError> {
    let result = result.map_err(CommandError::from_error);
    let error = result.as_ref().err().cloned();
    *app.state::<Desktop>().error.lock().unwrap() = error.clone();
    match error {
        Some(e) => {
            tray::present(app);
            Err(e)
        }
        None => result,
    }
}

pub(super) fn app_builder<R: tauri::Runtime>(
    builder: tauri::Builder<R>,
    background: bool,
) -> tauri::Builder<R> {
    commands::register(builder)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .setup(move |app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let item = tray::build(app)?;
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    let text =
                        tray::current_text(handle.state::<LanguageState>().settings().language);
                    let _ = item.set_text(text);
                    if let Some(icon) = handle.tray_by_id("xrun") {
                        let _ = icon.set_tooltip(Some(text));
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
            });
            if !background && !xrun::client::local_status()?.joined {
                tray::present(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                let _ = window.emit("xrun-window-visible", false);
            } else if matches!(
                event,
                tauri::WindowEvent::Focused(_) | tauri::WindowEvent::Resized(_)
            ) && let (Ok(visible), Ok(minimized)) =
                (window.is_visible(), window.is_minimized())
            {
                let _ = window.emit("xrun-window-visible", visible && !minimized);
            }
        })
}

pub(super) fn run_event<R: tauri::Runtime>(app: &tauri::AppHandle<R>, event: tauri::RunEvent) {
    #[cfg(target_os = "macos")]
    if let tauri::RunEvent::Reopen { .. } = event {
        tray::present(app);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, event);
}

#[cfg(all(test, target_os = "macos"))]
pub(crate) mod tests {
    use super::super::{
        commands::tests::{relay_config, seed},
        tray::menu_event,
    };
    use super::*;
    use anyhow::Result;
    use serde_json::Value;
    use std::{
        process::Command,
        time::{Duration, Instant},
    };
    use tauri::test::{mock_context, noop_assets};
    use xrun::testing::config;
    #[cfg(target_os = "macos")]
    #[allow(dead_code)] // Called by the native_ui target, which runs on the process main thread.
    pub(crate) mod native_ui {
        use super::*;
        use std::{os::unix::fs::PermissionsExt, process::Stdio};

        pub(crate) fn run() -> Result<()> {
            if let Ok(scenario) = std::env::var("XRUN_UI_SCENARIO") {
                if scenario.starts_with("self-check") {
                    super::super::super::main();
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
                        r#"<?xml version="1.0"?>
                        <plist version="1.0"><dict>
                            <key>CFBundleIdentifier</key>
                            <string>dev.qczone.xrun.native-tests</string>
                            <key>CFBundleExecutable</key><string>ui-test</string>
                        </dict></plist>"#,
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
                    super::super::super::tray::tests::verify_language_switch(app).unwrap();
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
                                let missing_helper = app
                                    .state::<Desktop>()
                                    .error
                                    .lock()
                                    .unwrap()
                                    .as_ref()
                                    .is_some_and(|e| e.code == "HELPER_NOT_FOUND");
                                let setup_visible = app
                                    .get_webview_window("main")
                                    .unwrap()
                                    .is_visible()
                                    .unwrap();
                                if (joined && missing_helper) || (!joined && setup_visible) {
                                    break;
                                }
                                tokio::time::sleep(Duration::from_millis(20)).await;
                            }
                        })
                        .await
                        .unwrap();
                        if !joined {
                            assert!(app.state::<Desktop>().error.lock().unwrap().is_none());
                            let directory = config::device_dir().unwrap();
                            assert!(!directory.join("identity.toml").exists());
                            assert!(!directory.join("daemon.initialized").exists());
                        }
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
