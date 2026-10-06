#[cfg(windows)]
use anyhow::Context;
use anyhow::{Result, bail};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Serialize)]
pub struct ServiceStatus {
    pub installed: bool,
    pub approval_required: bool,
    pub app_at_login: bool,
    pub legacy_installed: bool,
    pub development: bool,
}

pub fn helper() -> Result<PathBuf> {
    let path =
        std::env::current_exe()?.with_file_name(if cfg!(windows) { "xrun.exe" } else { "xrun" });
    if !path.is_file() {
        bail!("HELPER_NOT_FOUND: reinstall the xrun application");
    }
    Ok(path)
}

pub async fn check_helper() -> Result<()> {
    let helper = helper()?;
    let mut cmd = tokio::process::Command::new(&helper);
    cmd.arg("--version");
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let out = cmd.output().await?;
    if !out.status.success()
        || String::from_utf8_lossy(&out.stdout).trim()
            != format!("xrun {}", xrun::protocol::VERSION)
    {
        bail!("HELPER_VERSION_MISMATCH: reinstall the xrun application");
    }
    Ok(())
}

pub async fn start() -> Result<()> {
    check_helper().await?;
    let helper = helper()?;
    let dir = xrun::config::device_dir()?;
    if xrun::config::instance_running(&dir.join("daemon.lock"))? {
        // Opening the App never replaces or interrupts an already-running CLI daemon.
        return Ok(());
    }
    xrun::config::Identity::load()?;
    xrun::daemon::init()?;
    start_impl(&helper).await
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use objc2_foundation::NSString;
    use objc2_service_management::SMAppService;

    pub const LABEL: &str = xrun::service::APP_DAEMON_LABEL;

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
                legacy_installed: xrun::service::cli_daemon_installed()?,
                development: true,
            });
        }
        // These services are resolved relative to the calling App's main bundle.
        let agent = unsafe {
            SMAppService::agentServiceWithPlistName(&NSString::from_str(
                "dev.qczone.xrun.daemon.plist",
            ))
        };
        let main = unsafe { SMAppService::mainAppService() };
        let agent_status = unsafe { agent.status() }.0;
        Ok(ServiceStatus {
            installed: matches!(agent_status, 1 | 2),
            approval_required: agent_status == 2 || unsafe { main.status() }.0 == 2,
            app_at_login: matches!(unsafe { main.status() }.0, 1 | 2),
            legacy_installed: xrun::service::cli_daemon_installed().unwrap_or(false),
            development: false,
        })
    }

    pub fn register_agent() -> Result<()> {
        let exe = std::env::current_exe()?;
        if !is_bundle_executable(&exe) {
            bail!("APP_BUNDLE_REQUIRED: run the packaged xrun.app");
        }
        let service = unsafe {
            SMAppService::agentServiceWithPlistName(&NSString::from_str(
                "dev.qczone.xrun.daemon.plist",
            ))
        };
        if unsafe { service.status() }.0 != 1 {
            unsafe { service.registerAndReturnError() }
                .map_err(|e| anyhow::anyhow!("SERVICE_FAILED: {e}"))?;
        }
        if unsafe { service.status() }.0 == 2 {
            bail!("APPROVAL_REQUIRED: allow xrun in System Settings > General > Login Items");
        }
        Ok(())
    }

    pub fn unregister_agent() -> Result<()> {
        let service = unsafe {
            SMAppService::agentServiceWithPlistName(&NSString::from_str(
                "dev.qczone.xrun.daemon.plist",
            ))
        };
        if matches!(unsafe { service.status() }.0, 1 | 2) {
            unsafe { service.unregisterAndReturnError() }
                .map_err(|e| anyhow::anyhow!("SERVICE_FAILED: {e}"))?;
        }
        Ok(())
    }

    pub fn autostart(enabled: bool) -> Result<()> {
        if !is_bundle_executable(&std::env::current_exe()?) {
            bail!("APP_BUNDLE_REQUIRED: login startup requires the packaged xrun.app");
        }
        let service = unsafe { SMAppService::mainAppService() };
        if enabled && !matches!(unsafe { service.status() }.0, 1 | 2) {
            unsafe { service.registerAndReturnError() }
                .map_err(|e| anyhow::anyhow!("SERVICE_FAILED: {e}"))?;
        } else if !enabled && matches!(unsafe { service.status() }.0, 1 | 2) {
            unsafe { service.unregisterAndReturnError() }
                .map_err(|e| anyhow::anyhow!("SERVICE_FAILED: {e}"))?;
        }
        Ok(())
    }

    pub async fn start_dev_daemon(helper: &std::path::Path, dir: &std::path::Path) -> Result<()> {
        use std::{
            os::unix::fs::OpenOptionsExt, os::unix::fs::PermissionsExt, process::Stdio,
            time::Duration,
        };
        let log_path = dir.join("daemon-dev.log");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&log_path)?;
        log.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let previous = xrun::control::state(dir)?.map(|state| state.generation);
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
                    bail!(
                        "SERVICE_FAILED: daemon exited ({code}); see {}",
                        log_path.display()
                    );
                }
                if xrun::config::instance_running(&dir.join("daemon.lock"))?
                    && xrun::control::state(dir)?
                        .is_some_and(|state| Some(state.generation) != previous)
                {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            bail!("DAEMON_START_TIMEOUT: see {}", log_path.display());
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

        #[tokio::test]
        async fn dev_daemon_starts_detached_and_accepts_graceful_stop() -> Result<()> {
            let dir = tempfile::tempdir()?;
            let source = dir.path().join("helper.rs");
            let helper = dir.path().join("helper");
            std::fs::write(&source, include_str!("../tests/fixtures/dev-daemon.rs"))?;
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
            xrun::config::atomic_private_write(
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
            assert!(xrun::config::instance_running(
                &dir.path().join("daemon.lock")
            )?);
            assert_ne!(
                xrun::control::state(dir.path())?.unwrap().generation,
                "stale"
            );
            assert_eq!(
                std::fs::metadata(dir.path().join("daemon-dev.log"))?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            xrun::control::request_shutdown(dir.path()).await?;
            tokio::time::timeout(Duration::from_secs(3), async {
                while xrun::config::instance_running(&dir.path().join("daemon.lock"))? {
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
}

#[cfg(target_os = "macos")]
pub fn status() -> Result<ServiceStatus> {
    mac::state()
}
#[cfg(target_os = "macos")]
async fn start_impl(helper: &std::path::Path) -> Result<()> {
    if mac::development()? {
        return mac::start_dev_daemon(helper, &xrun::config::device_dir()?).await;
    }
    // Explicit Start migrates the legacy CLI LaunchAgent only after it has stopped.
    mac::register_agent()?;
    // Preserve the existing registration if the App signature or approval fails.
    xrun::service::uninstall("daemon").await?;
    let domain = format!("gui/{}/{}", unsafe { libc::getuid() }, mac::LABEL);
    let out = tokio::process::Command::new("launchctl")
        .args(["kickstart", &domain])
        .output()
        .await?;
    if !out.status.success() {
        bail!("SERVICE_FAILED: {}", String::from_utf8_lossy(&out.stderr));
    }
    Ok(())
}
#[cfg(target_os = "macos")]
pub async fn remove() -> Result<()> {
    xrun::service::stop_daemon().await?;
    if !mac::development()? {
        mac::unregister_agent()?;
    }
    xrun::service::uninstall("daemon").await
}
#[cfg(target_os = "macos")]
pub fn autostart(enabled: bool) -> Result<()> {
    mac::autostart(enabled)
}

#[cfg(windows)]
pub async fn prepare_uninstall() -> Result<()> {
    let helper = helper()?;
    // Do not remove a separate CLI installation that happens to share the data directory.
    let output = tokio::process::Command::new("powershell.exe")
        // An absent task is normal. Query by name reports an error when none exists.
        .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $t=Get-ScheduledTask | Where-Object { $_.TaskPath -eq '\\' -and $_.TaskName -eq 'xrun-daemon' }; if ($t -and $t.Actions.Execute -eq $env:XRUN_SERVICE_EXE) { Write-Output owned }; exit 0"])
        .env("XRUN_SERVICE_EXE", &helper).creation_flags(0x08000000).output().await?;
    if !output.status.success() {
        bail!(
            "SERVICE_FAILED: could not inspect the installed task: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    if String::from_utf8_lossy(&output.stdout).trim() == "owned" {
        remove().await?;
    }
    let key = startup_key()?;
    let own_value = format!("\"{}\" --background", std::env::current_exe()?.display());
    if key.get_value::<String, _>("xrun").ok().as_deref() == Some(&own_value) {
        key.delete_value("xrun")?;
    }
    Ok(())
}

#[cfg(windows)]
fn startup_key() -> Result<winreg::RegKey> {
    use winreg::{RegKey, enums::*};
    RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
        .map(|(key, _)| key)
        .context("open login startup settings")
}
#[cfg(windows)]
pub fn status() -> Result<ServiceStatus> {
    Ok(ServiceStatus {
        installed: xrun::service::installed("daemon")?,
        approval_required: false,
        app_at_login: startup_key()?.get_value::<String, _>("xrun").is_ok(),
        legacy_installed: false,
        development: false,
    })
}
#[cfg(windows)]
async fn start_impl(helper: &std::path::Path) -> Result<()> {
    // Register-ScheduledTask -Force updates the old CLI task's executable path.
    xrun::service::install_with_executable("daemon", helper).await
}
#[cfg(windows)]
pub async fn remove() -> Result<()> {
    xrun::service::stop_daemon().await?;
    xrun::service::uninstall("daemon").await
}
#[cfg(windows)]
pub fn autostart(enabled: bool) -> Result<()> {
    let key = startup_key()?;
    if enabled {
        key.set_value(
            "xrun",
            &format!("\"{}\" --background", std::env::current_exe()?.display()),
        )?;
    } else {
        match key.delete_value("xrun") {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn status() -> Result<ServiceStatus> {
    bail!("UNSUPPORTED_PLATFORM: desktop App requires macOS or Windows")
}
#[cfg(not(any(windows, target_os = "macos")))]
async fn start_impl(_helper: &std::path::Path) -> Result<()> {
    bail!("UNSUPPORTED_PLATFORM")
}
#[cfg(not(any(windows, target_os = "macos")))]
pub async fn remove() -> Result<()> {
    bail!("UNSUPPORTED_PLATFORM")
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn autostart(_enabled: bool) -> Result<()> {
    bail!("UNSUPPORTED_PLATFORM")
}
