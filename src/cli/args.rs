//! CLI syntax, options and argument validation.
use crate::protocol::*;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[cfg(test)]
mod tests;

#[derive(Parser)]
#[command(
    name = "xrun",
    version,
    about = "Run programs and transfer files on paired devices",
    after_help = "Remote: xrun <device> [options] -- <program> [args]\n        xrun <device> start|info|jobs|wait|logs|kill|push|pull|screenshot|forward\nUse xrun guide for examples."
)]
pub(super) struct LocalCli {
    #[arg(long, global = true)]
    pub(super) json: bool,
    #[command(subcommand)]
    pub(super) command: Local,
}
#[derive(Subcommand)]
pub(super) enum Local {
    /// Create an end-to-end network; this device becomes its manager
    Up {
        /// Complete HTTPS relay address or xrun-relay:// deployment link
        #[arg(long)]
        relay: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        no_daemon: bool,
        /// Grant mutual access to the joining device
        #[arg(long)]
        allow: bool,
    },
    /// Install or run the Linux ciphertext relay
    Relay {
        #[command(subcommand)]
        operation: RelayCommand,
    },
    /// Join a deployment using an invitation link
    Join {
        link: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        no_daemon: bool,
    },
    /// Create a ten-minute invitation (registration only by default)
    Invite {
        #[arg(long)]
        allow: bool,
    },
    /// Allow a source device to control this machine
    AllowFrom(PermissionArgs),
    /// Deny a source device access to this machine
    DenyFrom(PermissionArgs),
    /// Revoke a device identity (manager only)
    Revoke { device: String },
    /// Show local state and relay-reported device connections
    Status,
    /// Show this CLI's submissions from the last 24 hours
    Recent,
    /// Remove local services; optionally purge local data
    Down {
        #[arg(long)]
        purge: bool,
    },
    /// Internal service entry point for the Linux relay
    #[command(hide = true)]
    Server,
    /// Run the daemon or manage its service and task database
    Daemon {
        #[command(subcommand)]
        operation: Option<DaemonCommand>,
    },
    /// Print the README usage instructions
    Guide,
}
#[derive(Subcommand)]
pub(super) enum RelayCommand {
    Install {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long,action=clap::ArgAction::Append,value_delimiter=',')]
        addr: Vec<String>,
        #[arg(long)]
        no_detect: bool,
    },
    Run,
    /// Show the relay deployment link
    Invite,
    Uninstall,
}
#[derive(Subcommand)]
pub(super) enum DaemonCommand {
    Install,
    Uninstall,
    Start,
    Stop,
    Reset,
    /// Pause remote access; accepted reliable jobs continue running
    Pause,
    /// Resume remote access using the existing permissions
    Resume,
}
#[derive(Args)]
pub(super) struct PermissionArgs {
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub(super) device: Option<String>,
    /// Include current and future deployment members
    #[arg(long)]
    pub(super) all: bool,
}
#[derive(Parser)]
#[command(name = "xrun", version)]
pub(super) struct DeviceCli {
    #[arg(value_parser=device_selector)]
    pub(super) device: String,
    #[arg(long, global = true)]
    pub(super) json: bool,
    #[command(subcommand)]
    pub(super) command: Remote,
}
#[derive(Subcommand)]
pub(super) enum Remote {
    #[command(hide = true)]
    Run(Execute),
    /// Start a background job and return its reference
    Start(Execute),
    /// Show the device's last reported information
    Info,
    /// List jobs, or show one job's details
    Jobs {
        id: Option<String>,
        #[arg(long)]
        running: bool,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Wait for a job result and print its last output lines
    Wait {
        id: String,
        #[arg(long, default_value_t = 0)]
        timeout: u64,
        #[arg(long, default_value_t = 40)]
        tail: usize,
    },
    /// Read job output or follow new output
    Logs {
        id: String,
        #[arg(long)]
        follow: bool,
        #[arg(long, conflicts_with = "tail", default_value_t = 0)]
        after: u64,
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Cancel a job and wait for confirmation
    Kill { id: String },
    /// Upload one file (LOCAL REMOTE); use - for local stdin
    Push {
        local: String,
        remote: String,
        #[arg(short = 'C')]
        cwd: Option<String>,
        #[arg(long)]
        mkdir: bool,
        #[arg(long)]
        no_overwrite: bool,
        #[arg(long,conflicts_with="no_overwrite",value_parser=expect_hash)]
        expect: Option<String>,
    },
    /// Download one file (REMOTE LOCAL); use - for local stdout
    Pull {
        remote: String,
        local: Option<String>,
        #[arg(short = 'C')]
        cwd: Option<String>,
    },
    /// Capture the main display as PNG
    Screenshot { local: Option<PathBuf> },
    /// Forward a local loopback port to a device's loopback port
    Forward {
        #[arg(value_name = "[LOCAL:]REMOTE", value_parser = parse_ports)]
        ports: (u16, u16),
    },
}
#[derive(Args)]
pub(super) struct Execute {
    /// Remote working directory (daemon's default if omitted)
    #[arg(short = 'C')]
    pub(super) cwd: Option<String>,
    /// Override a remote environment variable (repeatable KEY=VALUE)
    #[arg(long,value_parser=parse_env)]
    pub(super) env: Vec<(String, String)>,
    /// Forward stdin bytes (maximum 1 MiB)
    #[arg(long, conflicts_with = "script")]
    pub(super) stdin: bool,
    /// Stream stdin/output for a connection-bound process (no saved job)
    #[arg(short = 'i', long = "interactive", conflicts_with_all = ["stdin", "request_id", "script"])]
    pub(super) interactive: bool,
    /// Read a UTF-8 script from stdin: sh, bash, zsh, powershell, pwsh or cmd
    #[arg(long)]
    pub(super) script: Option<String>,
    /// Seconds; 0 is unlimited (default: execution 1800, start 0)
    #[arg(long)]
    pub(super) timeout: Option<u64>,
    /// Retry the same submission using its original request ID
    #[arg(long)]
    pub(super) request_id: Option<String>,
    #[arg(last = true, required_unless_present = "script")]
    pub(super) command: Vec<String>,
}
fn expect_hash(value: &str) -> std::result::Result<String, String> {
    if crate::transfer::valid_hash(value) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err("expected a complete 64-digit SHA-256".into())
    }
}
fn device_selector(value: &str) -> std::result::Result<String, String> {
    if valid_name(value) {
        return Ok(value.into());
    }
    if let Some(id) = value.strip_prefix("dev_")
        && id.len() == 32
        && id.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Ok(format!("dev_{}", id.to_ascii_lowercase()));
    }
    Err("expected a device name or device ID".into())
}
fn parse_env(value: &str) -> std::result::Result<(String, String), String> {
    let (k, v) = value.split_once('=').ok_or("expected KEY=VALUE")?;
    if k.is_empty() || k.contains(['=', '\0']) || v.contains('\0') {
        return Err("invalid environment variable".into());
    }
    Ok((k.into(), v.into()))
}
fn parse_ports(value: &str) -> std::result::Result<(u16, u16), String> {
    let (local, remote) = value.split_once(':').unwrap_or((value, value));
    let local = local.parse::<u16>().map_err(|_| "invalid local port")?;
    let remote = remote.parse::<u16>().map_err(|_| "invalid remote port")?;
    if remote == 0 {
        return Err("remote port must be 1..65535".into());
    }
    Ok((local, remote))
}
