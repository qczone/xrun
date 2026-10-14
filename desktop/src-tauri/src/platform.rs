use super::{bridge, error};
use anyhow::{Context, Result, bail};
use serde::Serialize;
use serde_json::json;
use std::path::{Path, PathBuf};

#[derive(Serialize)]
pub struct ServiceStatus {
    pub installed: bool,
    pub approval_required: bool,
    pub app_at_login: bool,
    pub cli_service_installed: bool,
    pub development: bool,
}

/// Private data directory shared with the helper; it also holds App preferences.
pub fn data_dir() -> Result<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .context("HOME/USERPROFILE is unset")?;
    Ok(PathBuf::from(home).join(".xrun"))
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

/// The helper serving App operations. Windows runs the console CLI without a window;
/// `xrun.exe` beside the App is the windowless launcher used by the service.
#[cfg(not(test))]
pub fn bridge_helper() -> Result<PathBuf> {
    #[cfg(windows)]
    {
        let cli = std::env::current_exe()?
            .with_file_name("cli")
            .join("xrun.exe");
        if cli.is_file() {
            return Ok(cli);
        }
    }
    helper()
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
            != format!("xrun {}", env!("CARGO_PKG_VERSION"))
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
    prepare_service().await?;
    let helper = helper()?;
    if bridge::get("daemon_running", json!({})).await? {
        return Ok(());
    }
    bridge::call("initialize_daemon", json!({})).await?;
    start_service(&helper).await
}

pub async fn prepare_service() -> Result<()> {
    let restart: bool = bridge::get("daemon_restart_required", json!({})).await?;
    let pending: bool = bridge::get("daemon_restart_pending", json!({})).await?;
    if !restart && !pending {
        return Ok(());
    }
    // Validate the replacement before interrupting an existing service.
    check_helper().await?;
    let helper = helper()?;
    if restart {
        bridge::call("stop_daemon_for_restart", json!({})).await?;
    }
    if !bridge::get::<bool>("daemon_running", json!({})).await? {
        start_service(&helper).await?;
    }
    bridge::call("finish_daemon_restart", json!({})).await?;
    Ok(())
}

/// Readiness of the daemon owning `directory`, by default the shared data directory.
pub async fn daemon_state(directory: Option<&Path>) -> Result<bridge::DaemonState> {
    let directory = directory.map(|path| path.to_string_lossy());
    bridge::get("daemon_state", json!({ "directory": directory })).await
}

async fn start_service(helper: &Path) -> Result<()> {
    let previous = daemon_state(None).await?.generation;
    start_impl(helper).await?;
    for _ in 0..120 {
        let state = daemon_state(None).await?;
        if state.running && state.generation.is_some() && state.generation != previous {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!(error::failure(
        "DAEMON_START_TIMEOUT",
        "the service did not become ready; retry starting it"
    ))
}

#[cfg(target_os = "macos")]
#[path = "platform/cli_install.rs"]
mod cli_install;
#[cfg(target_os = "macos")]
#[path = "platform/macos.rs"]
mod mac;
#[cfg(windows)]
mod windows;
#[cfg(any(windows, test))]
#[path = "platform/windows_path.rs"]
mod windows_path;
#[cfg(windows)]
use windows::start_impl;
#[cfg(windows)]
pub(crate) use windows::{autostart, prepare_uninstall, remove, status};
#[cfg(windows)]
pub(crate) use windows_path::install_cli;

#[cfg(target_os = "macos")]
pub fn status() -> Result<ServiceStatus> {
    mac::state()
}
#[cfg(target_os = "macos")]
pub fn install_cli() -> Result<()> {
    let executable = std::env::current_exe()?;
    // A mounted DMG or Gatekeeper's temporary copy cannot back a lasting CLI link.
    if !mac::is_bundle_executable(&executable)
        || executable.starts_with("/Volumes")
        || executable
            .components()
            .any(|part| part.as_os_str() == "AppTranslocation")
    {
        return Ok(());
    }
    let directory = data_dir()?;
    let home = directory.parent().ok_or_else(|| {
        error::failure(
            "CLI_INSTALL_FAILED",
            "cannot locate the user home directory",
        )
    })?;
    let zsh_directory = std::env::var_os("ZDOTDIR").map(PathBuf::from);
    cli_install::install(&helper()?, home, zsh_directory.as_deref()).map_err(|failure| {
        error::failure(
            "CLI_INSTALL_FAILED",
            format!("could not configure the terminal xrun command: {failure:#}"),
        )
    })
}
#[cfg(target_os = "macos")]
async fn start_impl(helper: &Path) -> Result<()> {
    if mac::development()? {
        return mac::start_dev_daemon(helper, &data_dir()?).await;
    }
    // Explicit Start transfers a stopped CLI LaunchAgent to App ownership.
    mac::register_agent()?;
    // Preserve the existing registration if the App signature or approval fails.
    bridge::call("uninstall_daemon", json!({})).await?;
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
    bridge::call("stop_daemon", json!({})).await?;
    if !mac::development()? {
        mac::unregister_agent()?;
    }
    bridge::call("uninstall_daemon", json!({})).await?;
    Ok(())
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
async fn start_impl(_helper: &Path) -> Result<()> {
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
