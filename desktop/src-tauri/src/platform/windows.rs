//! Scheduled-task helper ownership and per-user application login startup.
use super::*;
pub(crate) async fn prepare_uninstall() -> Result<()> {
    let helper = helper()?;
    // Do not remove a separate CLI installation that happens to share the data directory.
    let output = tokio::process::Command::new("powershell.exe")
    // An absent task is normal. Query by name reports an error when none exists.
    .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; $t=Get-ScheduledTask | Where-Object { $_.TaskPath -eq '\\' -and $_.TaskName -eq 'xrun-daemon' }; if ($t -and $t.Actions.Execute -eq $env:XRUN_SERVICE_EXE) { Write-Output owned }; exit 0"])
    .env("XRUN_SERVICE_EXE", &helper).creation_flags(0x08000000).output().await?;
    if !output.status.success() {
        bail!(error::failure(
            "SERVICE_FAILED",
            format!(
                "could not inspect the installed task: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )
        ));
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

fn startup_key() -> Result<winreg::RegKey> {
    use winreg::{RegKey, enums::*};
    RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
        .map(|(key, _)| key)
        .context("open login startup settings")
}
pub(crate) fn status() -> Result<ServiceStatus> {
    Ok(ServiceStatus {
        installed: xrun::client::services::daemon_installed()?,
        approval_required: false,
        app_at_login: startup_key()?.get_value::<String, _>("xrun").is_ok(),
        legacy_installed: false,
        development: false,
    })
}
pub(crate) async fn start_impl(helper: &std::path::Path) -> Result<()> {
    // Register-ScheduledTask -Force updates the old CLI task's executable path.
    xrun::client::services::install_daemon_with_executable(helper).await
}
pub(crate) async fn remove() -> Result<()> {
    xrun::client::services::stop_daemon().await?;
    xrun::client::services::uninstall_daemon().await
}
pub(crate) fn autostart(enabled: bool) -> Result<()> {
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
