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
    prepare_upgrade().await?;
    let helper = helper()?;
    if xrun::client::services::daemon_running()? {
        return Ok(());
    }
    xrun::client::services::initialize_daemon()?;
    start_service(&helper).await
}

pub async fn prepare_upgrade() -> Result<()> {
    let (storage, restart, pending) = tokio::task::spawn_blocking(|| -> Result<_> {
        Ok((
            xrun::client::services::storage_upgrade_required()?,
            xrun::client::services::daemon_upgrade_required()?,
            xrun::client::services::daemon_upgrade_pending()?,
        ))
    })
    .await??;
    if !storage && !restart && !pending {
        return Ok(());
    }
    // Validate the replacement before interrupting an existing service.
    let helper = if restart || pending {
        check_helper().await?;
        Some(helper()?)
    } else {
        None
    };
    if restart {
        xrun::client::services::stop_daemon_for_upgrade().await?;
    }
    tokio::task::spawn_blocking(xrun::client::services::prepare_storage_upgrade).await??;
    if let Some(helper) = helper {
        if !xrun::client::services::daemon_running()? {
            start_service(&helper).await?;
        }
        xrun::client::services::finish_daemon_upgrade()?;
    }
    Ok(())
}

async fn start_service(helper: &std::path::Path) -> Result<()> {
    let directory = xrun::client::services::data_dir()?;
    let previous = xrun::client::services::daemon_state(&directory)?.generation;
    start_impl(helper).await?;
    for _ in 0..120 {
        let state = xrun::client::services::daemon_state(&directory)?;
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
    let directory = xrun::client::services::data_dir()?;
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
