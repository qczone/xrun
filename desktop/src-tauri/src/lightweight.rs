//! Release the webview while retaining the native tray and independent daemon.
use super::{
    app::{Desktop, DesktopPage},
    error::{self, CommandError},
    tray,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Mutex, time::Instant};
use tauri::{Emitter, Manager, State};

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Preferences {
    pub(super) enabled: bool,
    pub(super) delay_seconds: u16,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_seconds: 30,
        }
    }
}

#[derive(Default)]
struct WindowState {
    generation: u64,
    hidden_at: Option<Instant>,
    manual: bool,
    destroying: bool,
    reopen: bool,
    dialogs: usize,
}

pub(super) struct Lightweight {
    preferences: Mutex<Preferences>,
    window: Mutex<WindowState>,
}

impl Default for Lightweight {
    fn default() -> Self {
        let preferences = xrun::client::services::data_dir()
            .ok()
            .and_then(|directory| std::fs::read(directory.join("desktop-lightweight.json")).ok())
            .and_then(|bytes| serde_json::from_slice::<Preferences>(&bytes).ok())
            .filter(|value| value.delay_seconds <= 3600)
            .unwrap_or_default();
        Self {
            preferences: Mutex::new(preferences),
            window: Mutex::new(WindowState::default()),
        }
    }
}

impl Lightweight {
    fn save(&self, directory: &Path, preferences: Preferences) -> Result<()> {
        if preferences.delay_seconds > 3600 {
            anyhow::bail!(error::failure(
                "INVALID_CONFIG",
                "lightweight delay must be 0..3600 seconds"
            ));
        }
        let mut current = self.preferences.lock().unwrap();
        std::fs::create_dir_all(directory)?;
        let temporary = directory.join(".desktop-lightweight.tmp");
        let result = (|| -> Result<()> {
            std::fs::write(&temporary, serde_json::to_vec(&preferences)?)?;
            std::fs::rename(&temporary, directory.join("desktop-lightweight.json"))?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result?;
        *current = preferences;
        Ok(())
    }

    fn hidden(&self, manual: bool, now: Instant) {
        let mut window = self.window.lock().unwrap();
        window.generation += 1;
        window.hidden_at = Some(now);
        window.manual = manual;
    }

    pub(super) fn visible(&self) -> bool {
        let mut window = self.window.lock().unwrap();
        window.generation += 1;
        window.hidden_at = None;
        window.manual = false;
        if window.destroying {
            window.reopen = true;
            false
        } else {
            true
        }
    }

    fn pending(&self, now: Instant) -> Option<u64> {
        let preferences = *self.preferences.lock().unwrap();
        let window = self.window.lock().unwrap();
        let hidden_at = window.hidden_at?;
        (!window.destroying
            && window.dialogs == 0
            && (window.manual
                || (preferences.enabled
                    && now.duration_since(hidden_at).as_secs()
                        >= u64::from(preferences.delay_seconds))))
        .then_some(window.generation)
    }

    pub(super) fn dialog(&self) -> DialogGuard<'_> {
        self.window.lock().unwrap().dialogs += 1;
        DialogGuard(self)
    }
}

pub(super) struct DialogGuard<'a>(&'a Lightweight);

impl Drop for DialogGuard<'_> {
    fn drop(&mut self) {
        self.0.window.lock().unwrap().dialogs -= 1;
    }
}

#[tauri::command]
pub(super) fn lightweight_settings(state: State<'_, Lightweight>) -> Preferences {
    *state.preferences.lock().unwrap()
}

#[tauri::command]
pub(super) fn save_lightweight_settings(
    state: State<'_, Lightweight>,
    preferences: Preferences,
) -> Result<Preferences, CommandError> {
    state
        .save(
            &xrun::client::services::data_dir().map_err(CommandError::from_error)?,
            preferences,
        )
        .map_err(CommandError::from_error)?;
    Ok(preferences)
}

pub(super) fn hide<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    manual: bool,
) -> tauri::Result<()> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide()?;
        window.emit("xrun-window-visible", false)?;
        app.state::<Lightweight>().hidden(manual, Instant::now());
        tick(app);
    }
    Ok(())
}

#[tauri::command]
pub(super) fn enter_lightweight<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
) -> Result<(), CommandError> {
    hide(&app, true).map_err(CommandError::from_error)
}

/// All creation and destruction run on the native event thread (required by WebView2).
pub(super) fn present<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if !handle.state::<Lightweight>().visible() {
            return;
        }
        let result = (|| -> tauri::Result<()> {
            let window = match handle.get_webview_window("main") {
                Some(window) => window,
                None => {
                    let config = handle
                        .config()
                        .app
                        .windows
                        .iter()
                        .find(|window| window.label == "main")
                        .expect("main window configuration must exist");
                    tauri::WebviewWindowBuilder::from_config(&handle, config)?.build()?
                }
            };
            window.show()?;
            window.unminimize()?;
            window.emit("xrun-window-visible", true)?;
            window.set_focus()?;
            Ok(())
        })();
        if let Err(error) = result {
            *handle.state::<Desktop>().error.lock().unwrap() = Some(CommandError {
                code: "WINDOW_OPEN_FAILED".into(),
                message: error.to_string(),
            });
            eprintln!("could not open xrun window: {error}");
        }
        let _ = tray::refresh(&handle);
    });
}

/// Ask the renderer to acknowledge an idle point, including gaps between IPC calls.
pub(super) fn tick<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if app.state::<Lightweight>().pending(Instant::now()).is_none() {
        return;
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(generation) = handle.state::<Lightweight>().pending(Instant::now()) else {
            return;
        };
        if let Some(window) = handle.get_webview_window("main")
            && !window.is_visible().unwrap_or(true)
        {
            let _ = window.emit("xrun-lightweight-request", generation);
        }
    });
}

#[tauri::command]
pub(super) fn release_window<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    generation: u64,
    page: DesktopPage,
) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Err(error) = release(&handle, generation, page) {
            eprintln!("could not release xrun window: {error}");
        }
    });
}

fn release<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    generation: u64,
    page: DesktopPage,
) -> tauri::Result<bool> {
    let state = app.state::<Lightweight>();
    if state.pending(Instant::now()) != Some(generation) {
        return Ok(false);
    }
    let desktop = app.state::<Desktop>();
    let Ok(_guard) = desktop.action.try_lock() else {
        return Ok(false);
    };
    let Some(window) = app.get_webview_window("main") else {
        return Ok(false);
    };
    if window.is_visible()? {
        return Ok(false);
    }
    // A late acknowledgement cannot destroy a reopened or replacement window.
    {
        let mut window_state = state.window.lock().unwrap();
        window_state.destroying = true;
        window_state.hidden_at = None;
        window_state.generation += 1;
    }
    if let Err(error) = window.destroy() {
        state.window.lock().unwrap().destroying = false;
        state.hidden(false, Instant::now());
        return Err(error);
    }
    super::app::remember_page(app, page);
    Ok(true)
}

pub(super) fn destroyed<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    let reopen = {
        let state = app.state::<Lightweight>();
        let mut window = state.window.lock().unwrap();
        window.destroying = false;
        std::mem::take(&mut window.reopen)
    };
    if reopen {
        present(app);
    }
    let _ = tray::refresh(app);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;

    fn state() -> Lightweight {
        Lightweight {
            preferences: Mutex::new(Preferences::default()),
            window: Mutex::new(WindowState::default()),
        }
    }

    #[test]
    fn idle_deadline_cancels_on_reopen_and_manual_entry_overrides_disabled_automation() {
        let state = state();
        let now = Instant::now();
        state.hidden(false, now);
        assert!(state.pending(now + Duration::from_secs(29)).is_none());
        let old = state.pending(now + Duration::from_secs(30)).unwrap();
        assert!(state.visible());
        assert!(state.pending(now + Duration::from_secs(60)).is_none());
        state.hidden(false, now);
        let current = state.pending(now + Duration::from_secs(30)).unwrap();
        assert_ne!(old, current);
        state.preferences.lock().unwrap().enabled = false;
        assert!(state.pending(now + Duration::from_secs(600)).is_none());
        state.hidden(true, now);
        assert!(state.pending(now).is_some());
        let dialog = state.dialog();
        assert!(state.pending(now).is_none());
        drop(dialog);
        assert!(state.pending(now).is_some());
    }

    #[test]
    fn invalid_preferences_and_failed_writes_preserve_the_saved_settings() -> Result<()> {
        let state = state();
        let directory = tempfile::tempdir()?;
        let preferences = Preferences {
            enabled: false,
            delay_seconds: 60,
        };
        state.save(directory.path(), preferences)?;
        let file = directory.path().join("desktop-lightweight.json");
        let bytes = std::fs::read(&file)?;
        assert_eq!(serde_json::from_slice::<Preferences>(&bytes)?, preferences);
        assert!(
            state
                .save(
                    directory.path(),
                    Preferences {
                        delay_seconds: 3601,
                        ..preferences
                    }
                )
                .is_err()
        );
        let blocker = directory.path().join("file");
        std::fs::write(&blocker, "not a directory")?;
        assert!(state.save(&blocker, Preferences::default()).is_err());
        assert_eq!(*state.preferences.lock().unwrap(), preferences);
        assert_eq!(std::fs::read(file)?, bytes);
        assert!(!directory.path().join(".desktop-lightweight.tmp").exists());
        Ok(())
    }

    #[test]
    fn desktop_preferences_are_persisted_without_initializing_a_device() -> Result<()> {
        use super::super::commands::tests::{build_app, invoke, isolated};
        use serde_json::json;
        isolated(
            "desktop_preferences_are_persisted_without_initializing_a_device",
            || {
                let (app, window) = build_app();
                assert_eq!(
                    invoke(&window, "lightweight_settings", json!({})).unwrap(),
                    json!({"enabled":true,"delay_seconds":30})
                );
                let saved = json!({"enabled":false,"delay_seconds":60});
                assert_eq!(
                    invoke(
                        &window,
                        "save_lightweight_settings",
                        json!({"preferences":saved})
                    )
                    .unwrap(),
                    saved
                );
                assert_eq!(
                    *Lightweight::default().preferences.lock().unwrap(),
                    Preferences {
                        enabled: false,
                        delay_seconds: 60
                    }
                );
                let error = invoke(
                    &window,
                    "save_lightweight_settings",
                    json!({"preferences":{"enabled":true,"delay_seconds":3601}}),
                )
                .unwrap_err();
                assert_eq!(error["code"], "INVALID_CONFIG");
                assert_eq!(
                    invoke(&window, "lightweight_settings", json!({})).unwrap(),
                    saved
                );
                assert!(
                    invoke(
                        &window,
                        "release_window",
                        json!({"generation":0,"page":"unknown"})
                    )
                    .is_err()
                );
                let directory = xrun::client::services::data_dir()?;
                assert!(!directory.join("identity.toml").exists());
                assert!(!directory.join("daemon.db").exists());
                assert!(app.state::<Desktop>().error.lock().unwrap().is_none());
                Ok(())
            },
        )
    }

    #[cfg(target_os = "macos")]
    async fn native_step<T: Send + 'static>(
        app: &tauri::AppHandle,
        step: impl FnOnce(&tauri::AppHandle) -> T + Send + 'static,
    ) -> T {
        let handle = app.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        app.run_on_main_thread(move || {
            let _ = sender.send(step(&handle));
        })
        .unwrap();
        tokio::time::timeout(Duration::from_secs(5), receiver)
            .await
            .unwrap()
            .unwrap()
    }

    #[cfg(target_os = "macos")]
    async fn wait_for_window(app: &tauri::AppHandle, present: bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if app.get_webview_window("main").is_some() == present
                    && !app.state::<Lightweight>().window.lock().unwrap().destroying
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    // Called by native_ui on an isolated app's real Cocoa/Wry event loop.
    #[cfg(target_os = "macos")]
    pub(crate) fn exercise_native(app: tauri::AppHandle) {
        assert!(app.get_webview_window("main").is_none());
        tauri::async_runtime::spawn(async move {
            for _ in 0..3 {
                native_step(&app, |app| {
                    tray::present(app);
                    assert!(
                        app.get_webview_window("main")
                            .unwrap()
                            .is_visible()
                            .unwrap()
                    );
                    let state = app.state::<Lightweight>();
                    // Exercise the automatic deadline without waiting 30 real seconds.
                    hide(app, false).unwrap();
                    state.hidden(false, Instant::now() - Duration::from_secs(31));
                    let stale = state.pending(Instant::now()).unwrap();
                    tray::present(app);
                    assert!(!release(app, stale, DesktopPage::Activity).unwrap());
                    assert!(
                        app.get_webview_window("main")
                            .unwrap()
                            .is_visible()
                            .unwrap()
                    );
                    super::super::tray::menu_event(
                        app,
                        tauri::menu::MenuEvent {
                            id: "lightweight".into(),
                        },
                    );
                    let generation = state.pending(Instant::now()).unwrap();
                    let dialog = state.dialog();
                    assert!(!release(app, generation, DesktopPage::Activity).unwrap());
                    drop(dialog);
                    let desktop = app.state::<Desktop>();
                    let guard = desktop.action.try_lock().unwrap();
                    assert!(!release(app, generation, DesktopPage::Activity).unwrap());
                    drop(guard);
                    assert!(release(app, generation, DesktopPage::Activity).unwrap());
                    // An Open click racing the native destruction must reopen exactly once.
                    tray::present(app);
                })
                .await;
                wait_for_window(&app, true).await;
                native_step(&app, |app| {
                    let window = app.get_webview_window("main").unwrap();
                    assert!(window.is_visible().unwrap());
                    assert_eq!(app.webview_windows().len(), 1);
                    assert_eq!(
                        app.state::<Desktop>()
                            .navigation
                            .lock()
                            .unwrap()
                            .unwrap()
                            .page,
                        DesktopPage::Activity
                    );
                    let state = app.state::<Lightweight>();
                    state
                        .save(
                            &xrun::client::services::data_dir().unwrap(),
                            Preferences {
                                enabled: true,
                                delay_seconds: 0,
                            },
                        )
                        .unwrap();
                    hide(app, false).unwrap();
                    let generation = state.pending(Instant::now()).unwrap();
                    assert!(release(app, generation, DesktopPage::Settings).unwrap());
                })
                .await;
                wait_for_window(&app, false).await;
                assert!(app.tray_by_id("xrun").is_some());
                assert!(!xrun::client::local_status().unwrap().daemon_running);
                let directory = xrun::client::services::data_dir().unwrap();
                assert!(!directory.join("identity.toml").exists());
                assert!(!directory.join("daemon.db").exists());
            }
            app.exit(0);
        });
    }
}
