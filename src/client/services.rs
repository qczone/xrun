//! Daemon service operations; native desktop registration remains in its adapter.
use crate::{config, daemon, service};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// Label of the macOS daemon registered by the desktop application bundle.
#[cfg(target_os = "macos")]
pub const APP_DAEMON_LABEL: &str = service::APP_DAEMON_LABEL;

/// Private data directory used for daemon initialization and application diagnostics.
pub fn data_dir() -> Result<PathBuf> {
    config::device_dir()
}

/// Whether the local daemon currently holds its instance lock.
/// A process holding the lock is not necessarily connected to the relay.
pub fn daemon_running() -> Result<bool> {
    config::instance_running(&config::device_dir()?.join("daemon.lock"))
}

/// Readiness of a daemon in a private data directory, including instance identity.
pub struct DaemonState {
    /// Whether a daemon owns the directory's instance lock.
    pub running: bool,
    /// Generation published through the private control channel; absent before readiness.
    pub generation: Option<String>,
}

/// Probe the daemon in `dir`, normally returned by [`data_dir`]. Startup adapters
/// compare generations to avoid mistaking stale metadata for a newly started helper.
/// This reads local metadata only and does not connect to the relay or change files.
pub fn daemon_state(dir: &Path) -> Result<DaemonState> {
    Ok(DaemonState {
        running: config::instance_running(&dir.join("daemon.lock"))?,
        generation: crate::control::state(dir)?.map(|state| state.generation),
    })
}

/// Initialize local daemon storage for the saved identity, without launching it.
/// Only the current database layout is accepted. Registration may already be complete
/// if this fails, so adapters should offer a service retry.
pub fn initialize_daemon() -> Result<()> {
    daemon::init()
}

/// Whether a running daemon must switch to the application's current helper.
/// A different IPC version requires a graceful restart.
pub fn daemon_restart_required() -> Result<bool> {
    Ok(daemon_running()? && crate::ipc::version_matches(&config::device_dir()?)? == Some(false))
}

/// Whether service replacement still needs to resume a previously running daemon.
/// The private marker survives a crash or a failed storage/service retry.
pub fn daemon_restart_pending() -> Result<bool> {
    Ok(config::device_dir()?
        .join("daemon-restart-pending")
        .exists())
}

/// Remember that the daemon was running, then stop it gracefully for replacement.
/// Supervisor failures leave the resume marker available for retry.
pub async fn stop_daemon_for_restart() -> Result<()> {
    config::atomic_private_write(
        &config::device_dir()?.join("daemon-restart-pending"),
        b"resume\n",
    )?;
    service::stop_daemon().await
}

/// Clear the pending restart after successful startup or an explicit user stop.
pub fn finish_daemon_restart() -> Result<()> {
    let path = config::device_dir()?.join("daemon-restart-pending");
    match std::fs::remove_file(&path) {
        Ok(()) => config::sync_parent(&path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Stop the running daemon and wait for task cleanup. Does not unregister services,
/// delete identity or remove history. Already stopped daemons succeed immediately.
pub async fn stop_daemon() -> Result<()> {
    service::stop_daemon().await?;
    finish_daemon_restart()
}

/// Whether a supervisor registration exists for the current user's daemon.
pub fn daemon_installed() -> Result<bool> {
    service::installed("daemon")
}

/// Whether a separate CLI launch agent exists, for desktop service ownership checks.
#[cfg(target_os = "macos")]
pub fn cli_daemon_installed() -> Result<bool> {
    service::cli_daemon_installed()
}

/// Register and start the supplied same-version CLI helper as the user daemon.
/// The desktop executable itself must not be supplied. Supervisor errors propagate;
/// a written registration can remain for retry after the supervisor fails.
pub async fn install_daemon_with_executable(helper: &Path) -> Result<()> {
    service::install_with_executable("daemon", helper).await
}

/// Remove the daemon's supervisor registration. Stop the daemon first when cleanup
/// must complete before removal; this function preserves identity and task history.
pub async fn uninstall_daemon() -> Result<()> {
    service::uninstall("daemon").await
}
