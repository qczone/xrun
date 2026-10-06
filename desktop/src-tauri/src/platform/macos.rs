//! Bundle service registration, approval policy and detached development helper.
use super::*;
use objc2_foundation::NSString;
use objc2_service_management::SMAppService;

pub const LABEL: &str = xrun::client::services::APP_DAEMON_LABEL;

// Keep native calls at this boundary so approval and failure handling can be
// verified without registering a service in the test user's login session.
trait AppService {
    fn state(&self) -> isize;
    fn register(&self) -> Result<()>;
    fn unregister(&self) -> Result<()>;
}

impl AppService for SMAppService {
    fn state(&self) -> isize {
        unsafe { self.status() }.0
    }
    fn register(&self) -> Result<()> {
        unsafe { self.registerAndReturnError() }
            .map_err(|e| error::failure("SERVICE_FAILED", format!("{e}")))
    }
    fn unregister(&self) -> Result<()> {
        unsafe { self.unregisterAndReturnError() }
            .map_err(|e| anyhow::anyhow!("SERVICE_FAILED: {e}"))
    }
}

fn service_state(
    agent: &impl AppService,
    main: &impl AppService,
    legacy_installed: bool,
) -> ServiceStatus {
    let agent_status = agent.state();
    let main_status = main.state();
    ServiceStatus {
        installed: matches!(agent_status, 1 | 2),
        approval_required: agent_status == 2 || main_status == 2,
        app_at_login: matches!(main_status, 1 | 2),
        legacy_installed,
        development: false,
    }
}

fn enable_agent(service: &impl AppService) -> Result<()> {
    if service.state() != 1 {
        service.register()?;
    }
    if service.state() == 2 {
        bail!(error::failure(
            "APPROVAL_REQUIRED",
            "allow xrun in System Settings > General > Login Items"
        ));
    }
    Ok(())
}

fn set_enabled(service: &impl AppService, enabled: bool) -> Result<()> {
    if enabled && !matches!(service.state(), 1 | 2) {
        service.register()?;
    } else if !enabled && matches!(service.state(), 1 | 2) {
        service.unregister()?;
    }
    Ok(())
}

fn is_bundle_executable(exe: &std::path::Path) -> bool {
    let Some(macos) = exe.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    macos.file_name().is_some_and(|name| name == "MacOS")
        && contents.file_name().is_some_and(|name| name == "Contents")
        && contents
            .parent()
            .and_then(|path| path.extension())
            .is_some_and(|ext| ext == "app")
}

pub fn development() -> Result<bool> {
    Ok(tauri::is_dev() && !is_bundle_executable(&std::env::current_exe()?))
}

pub fn state() -> Result<ServiceStatus> {
    if development()? {
        return Ok(ServiceStatus {
            installed: false,
            approval_required: false,
            app_at_login: false,
            legacy_installed: xrun::client::services::cli_daemon_installed()?,
            development: true,
        });
    }
    // These services are resolved relative to the calling App's main bundle.
    let agent = unsafe {
        SMAppService::agentServiceWithPlistName(&NSString::from_str("dev.qczone.xrun.daemon.plist"))
    };
    let main = unsafe { SMAppService::mainAppService() };
    Ok(service_state(
        &*agent,
        &*main,
        xrun::client::services::cli_daemon_installed().unwrap_or(false),
    ))
}

pub fn register_agent() -> Result<()> {
    let exe = std::env::current_exe()?;
    if !is_bundle_executable(&exe) {
        bail!(error::failure(
            "APP_BUNDLE_REQUIRED",
            "run the packaged xrun.app"
        ));
    }
    let service = unsafe {
        SMAppService::agentServiceWithPlistName(&NSString::from_str("dev.qczone.xrun.daemon.plist"))
    };
    enable_agent(&*service)
}

pub fn unregister_agent() -> Result<()> {
    let service = unsafe {
        SMAppService::agentServiceWithPlistName(&NSString::from_str("dev.qczone.xrun.daemon.plist"))
    };
    set_enabled(&*service, false)
}

pub fn autostart(enabled: bool) -> Result<()> {
    if !is_bundle_executable(&std::env::current_exe()?) {
        bail!(error::failure(
            "APP_BUNDLE_REQUIRED",
            "login startup requires the packaged xrun.app"
        ));
    }
    let service = unsafe { SMAppService::mainAppService() };
    set_enabled(&*service, enabled)
}

pub async fn start_dev_daemon(helper: &std::path::Path, dir: &std::path::Path) -> Result<()> {
    use std::{
        os::unix::fs::OpenOptionsExt, os::unix::fs::PermissionsExt, process::Stdio, time::Duration,
    };
    let log_path = dir.join("daemon-dev.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&log_path)?;
    log.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    let previous = xrun::client::services::daemon_state(dir)?.generation;
    let mut command = tokio::process::Command::new(helper);
    command
        .arg("daemon")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .kill_on_drop(false);
    // A separate session keeps the daemon alive when the dev terminal closes.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let started = async {
        for _ in 0..50 {
            if let Some(code) = child.try_wait()? {
                bail!(error::failure(
                    "SERVICE_FAILED",
                    format!("daemon exited ({code}); see {}", log_path.display())
                ));
            }
            let state = xrun::client::services::daemon_state(dir)?;
            if state.running && state.generation.is_some() && state.generation != previous {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        bail!(error::failure(
            "DAEMON_START_TIMEOUT",
            format!("see {}", log_path.display())
        ));
    }
    .await;
    if started.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    started
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    mod service_policy {
        use super::*;
        use std::cell::{Cell, RefCell};

        struct Service {
            state: Cell<isize>,
            approval: bool,
            fail: bool,
            calls: RefCell<Vec<&'static str>>,
        }
        impl Service {
            fn new(state: isize) -> Self {
                Self {
                    state: Cell::new(state),
                    approval: false,
                    fail: false,
                    calls: RefCell::new(vec![]),
                }
            }
        }
        impl AppService for Service {
            fn state(&self) -> isize {
                self.state.get()
            }
            fn register(&self) -> Result<()> {
                self.calls.borrow_mut().push("register");
                if self.fail {
                    bail!(error::failure(
                        "SERVICE_FAILED",
                        "simulated registration error"
                    ));
                }
                self.state.set(if self.approval { 2 } else { 1 });
                Ok(())
            }
            fn unregister(&self) -> Result<()> {
                self.calls.borrow_mut().push("unregister");
                if self.fail {
                    bail!(error::failure("SERVICE_FAILED", "simulated removal error"));
                }
                self.state.set(0);
                Ok(())
            }
        }

        #[test]
        fn approval_and_failed_registration_never_report_a_running_service() -> Result<()> {
            for agent in 0..=3 {
                for main in 0..=3 {
                    let status = service_state(&Service::new(agent), &Service::new(main), true);
                    assert_eq!(status.installed, matches!(agent, 1 | 2));
                    assert_eq!(status.app_at_login, matches!(main, 1 | 2));
                    assert_eq!(status.approval_required, agent == 2 || main == 2);
                    assert!(status.legacy_installed && !status.development);
                }
            }
            let mut service = Service::new(0);
            service.fail = true;
            assert!(
                enable_agent(&service)
                    .unwrap_err()
                    .to_string()
                    .starts_with("SERVICE_FAILED:")
            );
            assert_eq!(service.state(), 0);
            service.fail = false;
            service.approval = true;
            assert!(
                enable_agent(&service)
                    .unwrap_err()
                    .to_string()
                    .starts_with("APPROVAL_REQUIRED:")
            );
            assert_eq!(service.state(), 2);
            service.state.set(1);
            service.calls.borrow_mut().clear();
            enable_agent(&service)?;
            assert!(
                service.calls.borrow().is_empty(),
                "already-enabled agents must not be registered again"
            );
            Ok(())
        }

        #[test]
        fn login_registration_is_idempotent_and_failed_removal_remains_registered() -> Result<()> {
            for initial in 0..=3 {
                let mut service = Service::new(initial);
                set_enabled(&service, true)?;
                let calls = service.calls.borrow().len();
                set_enabled(&service, true)?;
                assert_eq!(service.calls.borrow().len(), calls);
                assert_eq!(calls, usize::from(!matches!(initial, 1 | 2)));
                service.fail = true;
                assert!(set_enabled(&service, false).is_err());
                assert!(matches!(service.state(), 1 | 2));
                service.fail = false;
                set_enabled(&service, false)?;
                assert_eq!(service.state(), 0);
                let calls = service.calls.borrow().len();
                set_enabled(&service, false)?;
                assert_eq!(service.calls.borrow().len(), calls);
            }
            for path in [
                "/",
                "xrun",
                "/tmp/MacOS/xrun",
                "/tmp/example/Contents/MacOS/xrun",
                "/tmp/example.app/Other/MacOS/xrun",
            ] {
                assert!(!is_bundle_executable(std::path::Path::new(path)), "{path}");
            }
            assert!(is_bundle_executable(std::path::Path::new(
                "/tmp/example.app/Contents/MacOS/xrun"
            )));
            assert!(
                register_agent()
                    .unwrap_err()
                    .to_string()
                    .starts_with("APP_BUNDLE_REQUIRED:")
            );
            Ok(())
        }
    }

    #[tokio::test]
    async fn dev_daemon_starts_detached_and_accepts_graceful_stop() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("helper.rs");
        let helper = dir.path().join("helper");
        std::fs::write(&source, include_str!("../../tests/fixtures/dev-daemon.rs"))?;
        let compiled = tokio::process::Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&helper)
            .output()
            .await?;
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        // Metadata left by a crashed daemon must not count as readiness.
        xrun::testing::config::atomic_private_write(
            &dir.path().join("daemon-runtime.json"),
            br#"{"generation":"stale","connected":false}"#,
        )?;
        start_dev_daemon(&helper, dir.path()).await?;
        let pid: i32 = std::fs::read_to_string(dir.path().join("pid"))?.parse()?;
        struct Cleanup(Option<i32>);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                if let Some(pid) = self.0 {
                    unsafe {
                        libc::kill(pid, libc::SIGTERM);
                    }
                }
            }
        }
        let mut cleanup = Cleanup(Some(pid));
        assert_eq!(unsafe { libc::getsid(pid) }, pid);
        assert!(xrun::testing::config::instance_running(
            &dir.path().join("daemon.lock")
        )?);
        assert_ne!(
            xrun::testing::control::state(dir.path())?
                .unwrap()
                .generation,
            "stale"
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("daemon-dev.log"))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        xrun::testing::control::request_shutdown(dir.path()).await?;
        tokio::time::timeout(Duration::from_secs(3), async {
            while xrun::testing::config::instance_running(&dir.path().join("daemon.lock"))? {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        // The fixture has exited; never signal a subsequently reused PID.
        cleanup.0 = None;
        let error = start_dev_daemon(std::path::Path::new("/usr/bin/false"), dir.path())
            .await
            .unwrap_err();
        assert!(error.to_string().starts_with("SERVICE_FAILED:"));
        Ok(())
    }
}
