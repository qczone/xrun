//! Process entry points, separate from user operations and internal transports.
#![deny(missing_docs)]
use anyhow::Result;
use std::path::PathBuf;

/// Run the CLI using process arguments and return its documented exit status.
/// Writes command results to stdout and diagnostics to stderr.
pub async fn cli() -> i32 {
    // Tokio's root future runs on the calling thread. Keep that small (Windows
    // gives main only 1 MiB) and poll command/TLS work on the normal worker pool.
    match tokio::spawn(crate::cli::run()).await {
        Ok(status) => status,
        Err(error) => {
            eprintln!("[xrun] command task failed: {error}");
            125
        }
    }
}

/// Run the local daemon until shutdown, owning its instance lock and task cleanup.
/// Requires initialized identity and storage; concurrent instances are rejected.
pub async fn daemon() -> Result<()> {
    crate::daemon::run().await
}

/// Explicit options for a foreground relay process, without exposing config storage.
pub struct RelayOptions {
    /// Listening TCP port.
    pub port: u16,
    /// Advertised hostname or IP addresses including explicit ports.
    pub addresses: Vec<String>,
    /// Whether addresses were supplied explicitly.
    pub manual: bool,
    /// Disable automatic address discovery.
    pub no_detect: bool,
    /// Private directory holding the relay certificate and route secret.
    pub data_dir: PathBuf,
}

/// Run a relay with explicit options until shutdown. Creates missing private relay
/// keys in `data_dir`; invalid options and bind failures are returned to the caller.
pub async fn relay(options: RelayOptions) -> Result<()> {
    crate::relay::run(crate::config::ServerConfig {
        port: options.port,
        addresses: options.addresses,
        manual: options.manual,
        no_detect: options.no_detect,
        data_dir: options.data_dir,
    })
    .await
}
