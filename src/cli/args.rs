//! CLI syntax, options and argument validation.
use crate::protocol::*;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "xrun",
    bin_name = "xrun",
    version,
    about = "Run programs and transfer files on paired devices",
    disable_help_subcommand = true,
    subcommand_help_heading = "Local commands",
    after_help = concat!(
        "Remote commands (choose a device):\n",
        "  <DEVICE> [OPTIONS] -- <PROGRAM> [ARGS]...  Run a reliable task\n",
        "  <DEVICE> start [OPTIONS] -- <PROGRAM>...  Start a background task\n",
        "  <DEVICE> info                            Query device information\n",
        "  <DEVICE> jobs|wait|logs|kill              Manage your tasks on that device\n",
        "  <DEVICE> push|pull|screenshot             Transfer a file or capture the display\n",
        "  <DEVICE> forward [LOCAL:]REMOTE          Forward a TCP port\n",
        "\n",
        "Examples:\n",
        "  xrun linux1 -C /home/user/demo -- cargo test\n",
        "  xrun linux1 push ./config.json /home/user/demo/config.json\n",
        "  xrun allow-from mac1                    Run on the device mac1 will control\n",
        "\n",
        "Command help: xrun help push; xrun linux1 push --help\n",
        "Offline manual: xrun doc; xrun doc --list"
    )
)]
pub(super) struct LocalCli {
    /// Print structured results for supported operations
    #[arg(long, global = true)]
    pub(super) json: bool,
    #[command(subcommand)]
    pub(super) command: Local,
}
#[derive(Subcommand)]
pub(super) enum Local {
    /// Create a network; this device becomes its manager
    #[command(
        after_help = concat!(
        "Example: xrun up --relay '<complete relay address>' --name mac1\n",
        "The returned member invitation is used by xrun join.\n",
        "Full documentation: xrun doc quickstart"
    )
    )]
    Up {
        /// Complete HTTPS relay address or xrun-relay:// deployment link
        #[arg(long)]
        relay: String,
        /// Name for this device
        #[arg(long)]
        name: Option<String>,
        /// Prepare identity and task storage without installing a service
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
    /// Join a network using a one-use invitation
    #[command(
        after_help = concat!(
        "Example: xrun join '<complete xrun:// invitation>' --name linux1\n",
        "The manager must be online. Allow a caller on this device with xrun allow-from <SOURCE>.\n",
        "Full documentation: xrun doc quickstart"
    )
    )]
    Join {
        /// Complete xrun:// member invitation
        link: String,
        /// Name for this device
        #[arg(long)]
        name: Option<String>,
        /// Prepare identity and task storage without installing a service
        #[arg(long)]
        no_daemon: bool,
    },
    /// Create a ten-minute invitation (registration only by default)
    #[command(
        after_help = concat!(
        "Examples: xrun invite; xrun invite --allow\n",
        "Only the manager can invite. --allow grants mutual access between the manager and the joining device.\n",
        "Full documentation: xrun doc access"
    )
    )]
    Invite {
        /// Grant mutual access between the manager and joining device
        #[arg(long)]
        allow: bool,
    },
    /// Allow a source device to control this machine
    #[command(
        after_help = concat!(
        "Example: xrun allow-from mac1\n",
        "Run this on the device mac1 will control. --all trusts current and future members; individual denials take precedence.\n",
        "Full documentation: xrun doc access"
    )
    )]
    AllowFrom(PermissionArgs),
    /// Deny a source device access to this machine
    #[command(
        after_help = concat!(
        "Example: xrun deny-from mac1\n",
        "--all disables all-member trust while retaining individual permissions.\n",
        "Full documentation: xrun doc access"
    )
    )]
    DenyFrom(PermissionArgs),
    /// Revoke a device identity (manager only)
    Revoke { device: String },
    /// Show local state and relay-reported device connections
    #[command(
        after_help = concat!(
        "Example: xrun status --json\n",
        "For live details and reachability, use xrun <DEVICE> info. Offline timestamps are not retained.\n",
        "Full documentation: xrun doc errors"
    )
    )]
    Status,
    /// Show this CLI's submissions from the last 24 hours
    Recent,
    /// Remove local services; optionally purge local data
    Down {
        /// Delete local data after terminal confirmation
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
    /// Print the full offline manual or one chapter
    #[command(
        after_help = "Examples: xrun doc; xrun doc --list; xrun doc access\nNo identity, network or running daemon is required."
    )]
    Doc {
        /// Chapter to read (omit for the complete manual)
        #[arg(value_enum)]
        topic: Option<super::docs::Topic>,
        /// List available chapters
        #[arg(long, conflicts_with = "topic")]
        list: bool,
    },
    /// Show concise help for a local or remote command
    Help {
        /// Command path, such as push or relay install
        #[arg(value_name = "COMMAND", num_args = 0..)]
        command: Vec<String>,
    },
}
#[derive(Subcommand)]
pub(super) enum RelayCommand {
    /// Configure and start a Linux systemd user relay service
    Install {
        /// Listen port (9528 on first install; existing configuration retained)
        #[arg(long)]
        port: Option<u16>,
        /// Reachable HOST:PORT addresses (repeatable or comma-separated)
        #[arg(long,action=clap::ArgAction::Append,value_delimiter=',')]
        addr: Vec<String>,
        /// Disable automatic address detection
        #[arg(long)]
        no_detect: bool,
    },
    /// Run the configured relay in the foreground
    Run,
    /// Show the relay deployment link
    Invite,
    /// Remove the relay service, retaining local data
    Uninstall,
}
#[derive(Subcommand)]
pub(super) enum DaemonCommand {
    /// Install and start the current user's daemon service
    Install,
    /// Remove the service registration, retaining local data
    Uninstall,
    /// Start the installed daemon service
    Start,
    /// Stop the daemon and cancel its running tasks
    Stop,
    /// Rebuild task storage while stopped, retaining device identity
    Reset,
    /// Pause remote access; accepted reliable jobs continue running
    Pause,
    /// Resume remote access using the existing permissions
    Resume,
}
#[derive(Args)]
pub(super) struct PermissionArgs {
    /// Source device allowed or denied access to this machine
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub(super) device: Option<String>,
    /// Change all-member trust; individual permissions are retained
    #[arg(long)]
    pub(super) all: bool,
}
#[derive(Parser)]
#[command(
    name = "xrun",
    bin_name = "xrun",
    version,
    about = "Operate an authorized remote device",
    after_help = concat!(
        "Run a program: xrun <DEVICE> [OPTIONS] -- <PROGRAM> [ARGS]...\n",
        "Examples:\n",
        "  xrun linux1 -C /home/user/demo -- cargo test\n",
        "  xrun linux1 start -C /home/user/demo -- ./server\n",
        "Execution options: xrun help run\n",
        "Offline manual: xrun doc execute"
    )
)]
pub(super) struct DeviceCli {
    #[arg(value_parser=device_selector)]
    pub(super) device: String,
    /// Print structured results for supported operations
    #[arg(long, global = true)]
    pub(super) json: bool,
    #[command(subcommand)]
    pub(super) command: Remote,
}
#[derive(Subcommand)]
pub(super) enum Remote {
    #[command(
        hide = true,
        about = "Run a reliable task on the target device",
        override_usage = "xrun <DEVICE> [OPTIONS] -- <PROGRAM> [ARGS]...\n       xrun <DEVICE> [OPTIONS] --script <SHELL> < script",
        after_help = concat!(
        "Examples:\n",
        "  xrun linux1 -C /home/user/demo -- cargo test\n",
        "  xrun win1 -C 'D:\\demo' -- cargo build\n",
        "  xrun linux1 --script bash < ./build.sh\n",
        "Accepted tasks survive connection loss. Exit 75 means the result is unconfirmed; query the original task before retrying.\n",
        "Full documentation: xrun doc execute; xrun doc errors"
    )
    )]
    Run(Execute),
    /// Start a background job and return its reference
    #[command(
        after_help = concat!(
        "Example: xrun linux1 start -C /home/user/demo -- ./server\n",
        "Use the returned job reference with jobs, wait, logs or kill.\n",
        "Full documentation: xrun doc jobs"
    )
    )]
    Start(Execute),
    /// Query live device details (unavailable details are empty when offline)
    #[command(after_help = "Example: xrun linux1 info --json\nFull documentation: xrun doc errors")]
    Info,
    /// List jobs, or show one job's details
    #[command(
        after_help = concat!(
        "Examples:\n",
        "  xrun linux1 jobs --running\n",
        "  xrun linux1 jobs ABC123 --json\n",
        "  xrun linux1 jobs --request-id '<request_id>'\n",
        "Only tasks submitted by this device identity are visible.\n",
        "Full documentation: xrun doc jobs"
    )
    )]
    Jobs {
        /// Job ID or the returned DEVICE/ID reference
        id: Option<String>,
        /// Filter the list to running jobs
        #[arg(long)]
        running: bool,
        /// Find a submission by its request ID
        #[arg(long)]
        request_id: Option<String>,
        /// Maximum number of jobs per page
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Number of jobs to skip
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    /// Wait for a job result and print its last output lines
    #[command(
        after_help = concat!(
        "Example: xrun linux1 wait ABC123 --timeout 60\n",
        "Exit 75 means the result is unconfirmed. A wait timeout does not cancel the job.\n",
        "Full documentation: xrun doc jobs"
    )
    )]
    Wait {
        /// Job ID or the returned DEVICE/ID reference
        id: String,
        /// Wait seconds; 0 is unlimited, expiry does not cancel the task
        #[arg(long, default_value_t = 0)]
        timeout: u64,
        /// Last output lines to print
        #[arg(long, default_value_t = 40)]
        tail: usize,
    },
    /// Read job output or follow new output
    #[command(
        after_help = "Examples:\n  xrun linux1 logs ABC123 --tail 40\n  xrun linux1 logs ABC123 --follow\nFull documentation: xrun doc jobs"
    )]
    Logs {
        /// Job ID or the returned DEVICE/ID reference
        id: String,
        /// Keep reading new output after the current snapshot
        #[arg(long)]
        follow: bool,
        /// Read output after this log sequence number
        #[arg(long, conflicts_with = "tail", default_value_t = 0)]
        after: u64,
        /// Print the last N output lines
        #[arg(long)]
        tail: Option<usize>,
    },
    /// Cancel a job and wait for confirmation
    #[command(
        after_help = concat!(
        "Example: xrun linux1 kill ABC123\n",
        "Exit 75 means cancellation is unconfirmed; query the original job.\n",
        "Full documentation: xrun doc jobs"
    )
    )]
    Kill {
        /// Job ID or the returned DEVICE/ID reference
        id: String,
    },
    /// Upload one file (LOCAL REMOTE); use - for local stdin
    #[command(
        after_help = concat!(
        "Examples:\n",
        "  xrun linux1 push ./config.json /home/user/demo/config.json\n",
        "  xrun linux1 push ./result.txt /home/user/demo/output/result.txt --mkdir\n",
        "Files are limited to 64 MiB.\n",
        "Full documentation: xrun doc files"
    )
    )]
    Push {
        /// Local source path, or - for stdin
        local: String,
        /// Destination path on the target device
        remote: String,
        /// Absolute remote working directory for relative paths
        #[arg(short = 'C')]
        cwd: Option<String>,
        /// Create missing remote parent directories
        #[arg(long)]
        mkdir: bool,
        /// Fail if the remote destination already exists
        #[arg(long)]
        no_overwrite: bool,
        /// Overwrite only if the current remote SHA-256 matches
        #[arg(long,conflicts_with="no_overwrite",value_parser=expect_hash)]
        expect: Option<String>,
    },
    /// Download one file (REMOTE LOCAL); use - for local stdout
    #[command(
        after_help = concat!(
        "Examples:\n",
        "  xrun linux1 pull /home/user/demo/config.json ./config.json --json\n",
        "  xrun linux1 pull /home/user/demo/config.json -\n",
        "Files are limited to 64 MiB. Pulling to stdout cannot use --json.\n",
        "Full documentation: xrun doc files"
    )
    )]
    Pull {
        /// Source path on the target device
        remote: String,
        /// Local destination, - for stdout, or omit for a unique temporary file
        local: Option<String>,
        /// Absolute remote working directory for relative paths
        #[arg(short = 'C')]
        cwd: Option<String>,
    },
    /// Capture the main display as PNG
    #[command(
        after_help = concat!(
        "Example: xrun win1 screenshot ./screen.png --json\n",
        "Requires a usable desktop: macOS screen-recording permission, Windows interactive session, or Linux X11.\n",
        "Full documentation: xrun doc files"
    )
    )]
    Screenshot {
        /// Local PNG destination, or omit for a unique temporary file
        local: Option<PathBuf>,
    },
    /// Forward a local loopback port to a device's loopback port
    #[command(
        after_help = concat!(
        "Examples:\n",
        "  xrun linux1 forward 8080:3000\n",
        "  xrun linux1 forward 0:3000 --json\n",
        "Listens on local loopback; keep this command running. Ctrl+C closes the listener and its connections.\n",
        "Full documentation: xrun doc forward"
    )
    )]
    Forward {
        /// Remote loopback port; local defaults to the same port (0 selects one)
        #[arg(value_name = "[LOCAL:]REMOTE", value_parser = parse_ports)]
        ports: (u16, u16),
    },
}
#[derive(Args)]
pub(super) struct Execute {
    /// Absolute remote working directory (daemon's default if omitted)
    #[arg(short = 'C')]
    pub(super) cwd: Option<String>,
    /// Override a remote environment variable (repeatable KEY=VALUE)
    #[arg(long,value_parser=parse_env)]
    pub(super) env: Vec<(String, String)>,
    /// Forward stdin bytes (maximum 1 MiB)
    #[arg(long, conflicts_with = "script")]
    pub(super) stdin: bool,
    /// Stream stdin/output; no PTY or saved task, disconnect ends the process
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
    /// Program and arguments to execute on the target
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_hashes_and_environment_preserve_values_and_reject_ambiguous_input() {
        let id = format!("dev_{}", "AB".repeat(16));
        assert_eq!(device_selector(&id).unwrap(), id.to_ascii_lowercase());
        assert_eq!(device_selector("mac1").unwrap(), "mac1");
        for invalid in [
            "",
            "../mac1",
            "Mac 1",
            "dev_123",
            "dev_gggggggggggggggggggggggggggggggg",
        ] {
            assert!(device_selector(invalid).is_err(), "{invalid}");
        }
        let hash = "AB".repeat(32);
        assert_eq!(expect_hash(&hash).unwrap(), hash.to_ascii_lowercase());
        for invalid in ["abc", &"x".repeat(64), &"0".repeat(65)] {
            assert!(expect_hash(invalid).is_err());
        }
        assert_eq!(
            parse_env("TOKEN=a=b").unwrap(),
            ("TOKEN".into(), "a=b".into())
        );
        assert_eq!(
            parse_env("EMPTY=").unwrap(),
            ("EMPTY".into(), String::new())
        );
        for invalid in ["NO_EQUALS", "=value", "NA\0ME=value", "NAME=val\0ue"] {
            assert!(parse_env(invalid).is_err());
        }
    }
}
