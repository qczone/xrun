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

    pub const LABEL: &str = "dev.qczone.xrun.daemon";

    pub fn state() -> ServiceStatus {
        // These services are resolved relative to the calling App's main bundle.
        let agent = unsafe {
            SMAppService::agentServiceWithPlistName(&NSString::from_str(
                "dev.qczone.xrun.daemon.plist",
            ))
        };
        let main = unsafe { SMAppService::mainAppService() };
        let agent_status = unsafe { agent.status() }.0;
        ServiceStatus {
            installed: matches!(agent_status, 1 | 2),
            approval_required: agent_status == 2 || unsafe { main.status() }.0 == 2,
            app_at_login: matches!(unsafe { main.status() }.0, 1 | 2),
            legacy_installed: xrun::service::installed("daemon").unwrap_or(false),
        }
    }

    pub fn register_agent() -> Result<()> {
        let exe = std::env::current_exe()?;
        if exe
            .parent()
            .and_then(|p| p.file_name())
            .is_none_or(|p| p != "MacOS")
        {
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
}

#[cfg(target_os = "macos")]
pub fn status() -> Result<ServiceStatus> {
    Ok(mac::state())
}
#[cfg(target_os = "macos")]
async fn start_impl(_helper: &std::path::Path) -> Result<()> {
    // Explicit Start migrates the legacy CLI LaunchAgent only after it has stopped.
    xrun::service::uninstall("daemon").await?;
    mac::register_agent()?;
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
    mac::unregister_agent()?;
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
        .args(["-NoProfile", "-NonInteractive", "-Command", "$t=Get-ScheduledTask -TaskName xrun-daemon -ErrorAction SilentlyContinue; if ($t -and $t.Actions.Execute -eq $env:XRUN_SERVICE_EXE) { Write-Output owned }"])
        .env("XRUN_SERVICE_EXE", &helper).creation_flags(0x08000000).output().await?;
    if !output.status.success() {
        bail!("SERVICE_FAILED: could not inspect the installed task");
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
