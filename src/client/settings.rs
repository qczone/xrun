//! Execution settings with storage and identity checks owned by the core.
use crate::{config, config::Identity};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Fields editable without replacing permissions, environment or identity.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionSettings {
    /// Absolute default working directory; `None` uses the user's home.
    pub default_cwd: Option<PathBuf>,
    /// Maximum simultaneous persisted background jobs, between 1 and 64.
    pub max_concurrent_jobs: usize,
    /// Optional PATH override; `None` preserves the daemon's inherited PATH.
    pub path: Option<String>,
}

/// Execution settings and local paths used to explain their defaults.
#[derive(Serialize)]
pub struct Settings {
    /// Editable execution policy.
    pub execution: ExecutionSettings,
    /// Home directory used when no working directory is configured.
    pub home_dir: PathBuf,
    /// Private xrun data directory.
    pub data_dir: PathBuf,
    /// Rust platform name of the local machine.
    pub os: &'static str,
}

/// Read execution settings without requiring network membership or creating files.
/// Invalid configuration and filesystem failures are returned to the caller.
pub fn settings() -> Result<Settings> {
    let policy = config::DaemonConfig::load()?;
    Ok(Settings {
        execution: ExecutionSettings {
            default_cwd: policy.default_cwd,
            max_concurrent_jobs: policy.max_concurrent_jobs,
            path: policy
                .env
                .iter()
                .find(|(key, _)| {
                    if cfg!(windows) {
                        key.eq_ignore_ascii_case("PATH")
                    } else {
                        key.as_str() == "PATH"
                    }
                })
                .map(|(_, value)| value.clone()),
        },
        home_dir: config::home_dir()?,
        data_dir: config::device_dir()?,
        os: std::env::consts::OS,
    })
}

/// Validate and atomically save execution fields for an existing device identity.
/// Permission lists and unrelated environment variables are retained. New launches
/// use the saved policy; this does not restart or change existing tasks.
pub fn save_settings(execution: ExecutionSettings) -> Result<()> {
    Identity::load()?;
    config::update_execution(
        execution.default_cwd,
        execution.max_concurrent_jobs,
        execution.path,
    )
}
