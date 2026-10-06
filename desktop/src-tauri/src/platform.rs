use super::error;
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
        bail!(error::failure(
            "HELPER_NOT_FOUND",
            "reinstall the xrun application"
        ));
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
        bail!(error::failure(
            "HELPER_VERSION_MISMATCH",
            "reinstall the xrun application"
        ));
    }
    Ok(())
}

pub async fn start() -> Result<()> {
    check_helper().await?;
    let helper = helper()?;
    if xrun::client::services::daemon_running()? {
        // Opening the App never replaces or interrupts an already-running CLI daemon.
        return Ok(());
    }
    xrun::client::services::initialize_daemon()?;
    start_impl(&helper).await
}

#[cfg(target_os = "macos")]
#[path = "platform/macos.rs"]
mod mac;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::start_impl;
#[cfg(windows)]
pub(crate) use windows::{autostart, prepare_uninstall, remove, status};

#[cfg(target_os = "macos")]
pub fn status() -> Result<ServiceStatus> {
    mac::state()
}
#[cfg(target_os = "macos")]
async fn start_impl(helper: &std::path::Path) -> Result<()> {
    if mac::development()? {
        return mac::start_dev_daemon(helper, &xrun::client::services::data_dir()?).await;
    }
    // Explicit Start migrates the legacy CLI LaunchAgent only after it has stopped.
    mac::register_agent()?;
    // Preserve the existing registration if the App signature or approval fails.
    xrun::client::services::uninstall_daemon().await?;
    let domain = format!("gui/{}/{}", unsafe { libc::getuid() }, mac::LABEL);
    let out = tokio::process::Command::new("launchctl")
        .args(["kickstart", &domain])
        .output()
        .await?;
    if !out.status.success() {
        bail!(error::failure(
            "SERVICE_FAILED",
            format!("{}", String::from_utf8_lossy(&out.stderr))
        ));
    }
    Ok(())
}
#[cfg(target_os = "macos")]
pub async fn remove() -> Result<()> {
    xrun::client::services::stop_daemon().await?;
    if !mac::development()? {
        mac::unregister_agent()?;
    }
    xrun::client::services::uninstall_daemon().await
}
#[cfg(target_os = "macos")]
pub fn autostart(enabled: bool) -> Result<()> {
    mac::autostart(enabled)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn status() -> Result<ServiceStatus> {
    bail!("UNSUPPORTED_PLATFORM: desktop App requires macOS or Windows")
}
#[cfg(not(any(windows, target_os = "macos")))]
async fn start_impl(_helper: &std::path::Path) -> Result<()> {
    bail!(error::failure(
        "UNSUPPORTED_PLATFORM",
        "desktop App requires macOS or Windows"
    ))
}
#[cfg(not(any(windows, target_os = "macos")))]
pub async fn remove() -> Result<()> {
    bail!("UNSUPPORTED_PLATFORM")
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn autostart(_enabled: bool) -> Result<()> {
    bail!("UNSUPPORTED_PLATFORM")
}
