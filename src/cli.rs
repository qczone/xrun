use crate::error::ErrorCode;
use crate::{
    config::{self, Identity, ServerConfig},
    crypto, daemon,
    net::{self, Ws},
    protocol::*,
    service,
    session::Session,
    store::{Submission, SubmissionStore},
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{Args, Parser, Subcommand};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Parser)]
#[command(
    name = "xrun",
    version,
    about = "Run programs and transfer files on paired devices",
    after_help = "Remote: xrun <device> [options] -- <program> [args]\n        xrun <device> start|info|jobs|wait|logs|kill|push|pull|screenshot|forward\nUse xrun guide for examples."
)]
struct LocalCli {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Local,
}
#[derive(Subcommand)]
enum Local {
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
enum RelayCommand {
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
enum DaemonCommand {
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
struct PermissionArgs {
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    device: Option<String>,
    /// Include current and future deployment members
    #[arg(long)]
    all: bool,
}
#[derive(Parser)]
#[command(name = "xrun", version)]
struct DeviceCli {
    #[arg(value_parser=device_selector)]
    device: String,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Remote,
}
#[derive(Subcommand)]
enum Remote {
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
struct Execute {
    /// Remote working directory (daemon's default if omitted)
    #[arg(short = 'C')]
    cwd: Option<String>,
    /// Override a remote environment variable (repeatable KEY=VALUE)
    #[arg(long,value_parser=parse_env)]
    env: Vec<(String, String)>,
    /// Forward stdin bytes (maximum 1 MiB)
    #[arg(long, conflicts_with = "script")]
    stdin: bool,
    /// Stream stdin/output for a connection-bound process (no saved job)
    #[arg(short = 'i', long = "interactive", conflicts_with_all = ["stdin", "request_id", "script"])]
    interactive: bool,
    /// Read a UTF-8 script from stdin: sh, bash, zsh, powershell, pwsh or cmd
    #[arg(long)]
    script: Option<String>,
    /// Seconds; 0 is unlimited (default: execution 1800, start 0)
    #[arg(long)]
    timeout: Option<u64>,
    /// Retry the same submission using its original request ID
    #[arg(long)]
    request_id: Option<String>,
    #[arg(last = true, required_unless_present = "script")]
    command: Vec<String>,
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
fn print<T: Serialize>(json: bool, value: &T, text: impl FnOnce()) {
    if json {
        println!("{}", serde_json::to_string(value).unwrap())
    } else {
        text()
    }
}
fn diagnostic(json: bool, error: &anyhow::Error) {
    if json {
        eprintln!("{}", serde_json::to_string(&Data::error(error)).unwrap())
    } else {
        eprintln!("[xrun] {error:#}")
    }
}
fn require_linux() -> Result<()> {
    if !cfg!(target_os = "linux") {
        bail!(ErrorCode::UnsupportedPlatform.error("Server deployment requires Linux"))
    }
    Ok(())
}
pub async fn run() -> Result<i32> {
    let mut args: Vec<String> = std::env::args().collect();
    let first = args
        .iter()
        .skip(1)
        .position(|a| a != "--json")
        .map(|i| i + 1);
    if let Some(first) = first
        && !args[first].starts_with('-')
        && !RESERVED.contains(&args[first].as_str())
    {
        let mut position = first + 1;
        while args.get(position).is_some_and(|a| a == "--json") {
            position += 1;
        }
        let ops = [
            "start",
            "info",
            "jobs",
            "wait",
            "logs",
            "kill",
            "push",
            "pull",
            "screenshot",
            "forward",
        ];
        if args
            .get(position)
            .is_none_or(|a| !ops.contains(&a.as_str()))
        {
            args.insert(position, "run".into())
        }
        let cli = DeviceCli::parse_from(args);
        let json = cli.json;
        let file = matches!(
            &cli.command,
            Remote::Push { .. } | Remote::Pull { .. } | Remote::Screenshot { .. }
        );
        // Keep each command's async state on the heap; Windows has a smaller
        // default main-thread stack, especially visible in debug builds.
        return match Box::pin(remote(cli)).await {
            Ok(code) => Ok(code),
            Err(e) => {
                diagnostic(json, &e);
                Ok(if file && !net::explicit(&e) && !network_error(&e) {
                    1
                } else {
                    125
                })
            }
        };
    }
    let cli = LocalCli::parse_from(args);
    let json = cli.json;
    match Box::pin(local(cli)).await {
        Ok(code) => Ok(code),
        Err(e) => {
            diagnostic(json, &e);
            Ok(125)
        }
    }
}
async fn identity() -> Result<Identity> {
    let mut id = Identity::load()?;
    net::renew_identity(&mut id).await?;
    Ok(id)
}
async fn local(cli: LocalCli) -> Result<i32> {
    let json = cli.json;
    match cli.command {
        Local::Up {
            relay,
            name,
            no_daemon,
            allow,
        } => up(&relay, name, no_daemon, allow, json).await?,
        Local::Relay { operation } => match operation {
            RelayCommand::Install {
                port,
                addr,
                no_detect,
            } => relay_install(port, addr, no_detect, json).await?,
            RelayCommand::Run => run_relay().await?,
            RelayCommand::Invite => {
                require_linux()?;
                let link = crate::relay::deployment_link(&ServerConfig::load()?)?;
                print(json, &serde_json::json!({"link":link}), || {
                    println!("{link}")
                });
            }
            RelayCommand::Uninstall => {
                require_linux()?;
                service::uninstall("server").await?;
            }
        },
        Local::Join {
            link,
            name,
            no_daemon,
        } => {
            crate::client::join(&link, name).await?;
            let id = Identity::load()?;
            daemon::init()?;
            if !no_daemon {
                service::install("daemon").await?
            }
            print(
                json,
                &serde_json::json!({"device_id":id.device_id,"name":id.name}),
                || println!("{} ({})", id.name, id.device_id),
            );
        }
        Local::Invite { allow } => {
            let id = identity().await?;
            let value: serde_json::Value = net::http(
                &id,
                reqwest::Method::POST,
                "/invites",
                Some(serde_json::json!({"allow":allow})),
            )
            .await?;
            if value["allow"].as_bool() != Some(allow) {
                bail!(
                    ErrorCode::VersionMismatch
                        .error("invitation policy differs; upgrade all components")
                )
            }
            print(json, &value, || {
                println!("{}", value["link"].as_str().unwrap_or_default())
            });
            invitation_notice(allow);
        }
        Local::AllowFrom(args) => permission(args, true, json).await?,
        Local::DenyFrom(args) => permission(args, false, json).await?,
        Local::Revoke { device } => {
            let id = identity().await?;
            let v: serde_json::Value = net::http(
                &id,
                reqwest::Method::POST,
                "/admin/revoke",
                Some(serde_json::json!({"device":device})),
            )
            .await?;
            print(json, &v, || {
                println!(
                    "revoked {} (roster {})",
                    v["device_id"], v["roster_version"]
                );
                if let Some(devices) = v["undelivered"].as_array()
                    && !devices.is_empty()
                {
                    eprintln!(
                        "[xrun] not confirmed by: {}",
                        devices
                            .iter()
                            .filter_map(|d| d.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if let Some(error) = v["sync_error"].as_str() {
                    eprintln!(
                        "[xrun] signed revocation saved locally; peer synchronization failed: {error}"
                    );
                }
            });
        }
        Local::Status => {
            return status(json).await;
        }
        Local::Recent => {
            let submissions =
                SubmissionStore::open(&config::device_dir()?.join("submissions.sqlite"))?
                    .recent()?;
            print(json, &submissions, || {
                for s in &submissions {
                    println!(
                        "{}\t{}\t{}\t{}",
                        s.request_id,
                        s.job_id.as_deref().unwrap_or("-"),
                        s.target_device_id,
                        s.status
                    );
                }
            })
        }
        Local::Down { purge } => {
            service::uninstall("daemon").await?;
            if cfg!(target_os = "linux") {
                service::uninstall("server").await?
            }
            if purge {
                if !std::io::stdin().is_terminal() {
                    bail!(
                        ErrorCode::InteractiveRequired
                            .error("--purge requires terminal confirmation")
                    )
                };
                eprint!("Delete all local xrun identity, CA, jobs and logs? Type purge: ");
                std::io::stderr().flush()?;
                let mut value = String::new();
                std::io::stdin().read_line(&mut value)?;
                if value.trim() != "purge" {
                    return Ok(0);
                }
                let _lock = daemon::instance_lock()?;
                let _server_lock = ServerConfig::load()
                    .ok()
                    .map(|cfg| crate::server::instance_lock(&cfg.data_dir))
                    .transpose()?;
                std::fs::remove_dir_all(config::device_dir()?)?;
            }
        }
        Local::Server => run_relay().await?,
        Local::Daemon { operation } => match operation {
            None => daemon::run().await?,
            Some(DaemonCommand::Install) => {
                daemon::init()?;
                service::install("daemon").await?
            }
            Some(DaemonCommand::Uninstall) => service::uninstall("daemon").await?,
            Some(DaemonCommand::Start) => service::start("daemon").await?,
            Some(DaemonCommand::Stop) => service::stop_daemon().await?,
            Some(DaemonCommand::Reset) => daemon::reset()?,
            Some(DaemonCommand::Pause) => config::pause_remote_access(true)?,
            Some(DaemonCommand::Resume) => config::pause_remote_access(false)?,
        },
        Local::Guide => print!("{}", include_str!("../README.md")),
    }
    Ok(0)
}
async fn status(json: bool) -> Result<i32> {
    let status = crate::client::status().await?;
    print(json, &status, || {
        if let (Some(name), Some(id)) = (&status.local.name, &status.local.device_id) {
            println!("local: {name} ({id})");
        } else {
            println!("local: not joined");
        }
        println!(
            "daemon: {}",
            if status.local.daemon_running {
                "running"
            } else {
                "stopped"
            }
        );
        println!(
            "remote access: {}",
            if status.local.remote_access_paused {
                "paused"
            } else {
                "enabled"
            }
        );
        println!(
            "trust: {}",
            if status.local.allow_all {
                "all current and future members (individual denials apply)"
            } else {
                "individually allowed devices"
            }
        );
        if status.local.server_configured {
            println!(
                "server: {}",
                if status.local.server_running {
                    "running"
                } else {
                    "stopped"
                }
            );
        }
        if let Some(devices) = &status.devices {
            for d in devices {
                println!(
                    "{}\t{}\t{}",
                    d.name,
                    d.device_id,
                    if d.revoked {
                        "revoked"
                    } else if d.online {
                        "relay-connected"
                    } else {
                        "disconnected"
                    }
                );
            }
        }
        if let Some(error) = &status.server_error {
            eprintln!("[xrun] device list unavailable: {error:?}");
        }
    });
    Ok(if status.server_error.is_some() {
        125
    } else {
        0
    })
}
async fn permission(args: PermissionArgs, allow: bool, json: bool) -> Result<()> {
    if args.all {
        config::update_all_permissions(allow)?;
        print(
            json,
            &serde_json::json!({"all":true,"allowed":allow}),
            || {
                println!(
                    "all-member access {}; individual permissions retained",
                    if allow {
                        "enabled (includes future members)"
                    } else {
                        "disabled"
                    }
                );
            },
        );
        return Ok(());
    }
    let value = args
        .device
        .as_deref()
        .context(ErrorCode::InvalidRequest.error("device required"))?;
    let result = crate::client::set_permission(value, allow).await?;
    print(json, &result, || {
        println!(
            "{} {}",
            if allow { "allowed" } else { "denied" },
            result.source_device_id
        )
    });
    Ok(())
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

async fn detect_addresses(port: u16, no_detect: bool) -> Result<Vec<String>> {
    let mut addresses = vec![];
    #[cfg(unix)]
    unsafe {
        let mut first = std::ptr::null_mut();
        if libc::getifaddrs(&mut first) == 0 {
            let mut current = first;
            while !current.is_null() {
                let item = &*current;
                if !item.ifa_addr.is_null() && (*item.ifa_addr).sa_family as i32 == libc::AF_INET {
                    let addr = &*(item.ifa_addr as *const libc::sockaddr_in);
                    let ip = std::net::Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes());
                    if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() {
                        addresses.push(format!("{ip}:{port}"));
                    }
                }
                current = item.ifa_next;
            }
            libc::freeifaddrs(first);
        }
    }
    if !no_detect {
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
                .build()?;
            let value = client
                .get("https://api.ipify.org")
                .send()
                .await?
                .text()
                .await?;
            Ok::<_, anyhow::Error>(value.trim().parse::<std::net::Ipv4Addr>()?)
        })
        .await;
        if let Ok(Ok(ip)) = result {
            addresses.push(format!("{ip}:{port}"));
        }
    }
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        bail!(ErrorCode::NoAddress.error("provide --addr <host>:<port>"))
    }
    Ok(addresses)
}
async fn run_relay() -> Result<()> {
    require_linux()?;
    let mut cfg = ServerConfig::load()?;
    if !cfg.manual {
        cfg.addresses = detect_addresses(cfg.port, cfg.no_detect).await?;
        cfg.save()?;
    }
    tokio::select! {result=crate::relay::run(cfg)=>result,_=daemon::shutdown_signal()=>Ok(())}
}
async fn relay_install(
    port: Option<u16>,
    addresses: Vec<String>,
    no_detect: bool,
    json: bool,
) -> Result<()> {
    require_linux()?;
    let dir = config::device_dir()?;
    let before = ServerConfig::load().ok();
    let mut cfg = before.clone().unwrap_or(ServerConfig {
        port: port.unwrap_or(9528),
        addresses: vec![],
        manual: false,
        no_detect,
        data_dir: dir.join("server"),
    });
    if let Some(port) = port {
        cfg.port = port;
    }
    if cfg.port == 0 {
        bail!(ErrorCode::InvalidPort.error("port must be 1..65535"))
    }
    if !addresses.is_empty() {
        for address in &addresses {
            crate::client::validate_address(address)?;
        }
        cfg.addresses = addresses;
        cfg.manual = true;
    } else if !cfg.manual {
        cfg.no_detect |= no_detect;
        cfg.addresses = detect_addresses(cfg.port, cfg.no_detect).await?;
    }
    cfg.save()?;
    let link = crate::relay::deployment_link(&cfg)?;
    service::install("server").await?;
    if before.is_some_and(|old| old.port != cfg.port || old.addresses != cfg.addresses) {
        service::restart("server").await?;
    }
    print(
        json,
        &serde_json::json!({"link":link,"addresses":crate::relay::addresses(&cfg)?}),
        || {
            println!("relay: {}", cfg.addresses.join(", "));
            println!("xrun up --relay '{link}'");
        },
    );
    eprintln!(
        "[xrun] keep the relay link private; allow inbound TCP {}",
        cfg.port
    );
    Ok(())
}
async fn up(
    relay: &str,
    name: Option<String>,
    no_daemon: bool,
    allow: bool,
    json: bool,
) -> Result<()> {
    let id = crate::network::create(relay, name).await?;
    daemon::init()?;
    if !no_daemon {
        service::install("daemon").await?;
    }
    let invitation = crate::network::invite(&id, allow).await?;
    print(
        json,
        &serde_json::json!({"device_id":id.device_id,"network_id":crate::network::authority(&id)?.network_id,"link":invitation["link"],"addresses":id.addresses,"allow":allow}),
        || {
            println!("manager: {} ({})", id.name, id.device_id);
            println!(
                "xrun join '{}'",
                invitation["link"].as_str().unwrap_or_default()
            );
        },
    );
    invitation_notice(allow);
    Ok(())
}
fn invitation_notice(allow: bool) {
    eprintln!("[xrun] invitation is a secret, valid once for 10 minutes");
    if allow {
        eprintln!(
            "[xrun] --allow grants mutual command execution as the device's user; share only with a trusted device"
        );
    }
}

async fn session(id: &Identity, target: &str) -> Result<Session> {
    Session::open(id, target).await
}
async fn response(ws: &mut Ws) -> Result<Data> {
    match net::receive::<Data>(ws).await? {
        Data::Error { code, message } => bail!(crate::error::CodedError::from_wire(code, message)),
        value => Ok(value),
    }
}
async fn request(id: &Identity, target: &str, req: Request) -> Result<Data> {
    let mut s = session(id, target).await?;
    net::send(&mut s.ws, &Data::Request { request: req }).await?;
    let value = response(&mut s.ws).await?;
    s.finish().await;
    Ok(value)
}
fn job_ref(job: &Job) -> String {
    format!("{}/{}", job.target_device_id, job.job_id)
}
fn parse_job(value: &str, target: &str, name: &str) -> Result<String> {
    let id = if let Some((device, id)) = value.split_once('/') {
        if device != target && device != name {
            bail!(ErrorCode::InvalidJobRef.error("job belongs to another device"))
        }
        id
    } else {
        value
    };
    let id = id.to_ascii_uppercase();
    if id.len() != 6
        || !id
            .bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
    {
        bail!(ErrorCode::InvalidJobRef.error("expected a six-character job ID"))
    };
    Ok(id)
}
fn job_code(job: &Job) -> i32 {
    match job.state {
        JobState::TimedOut => 124,
        JobState::Canceled => 130,
        JobState::Failed | JobState::Lost => 125,
        _ => {
            if let Some(signal) = job.signal {
                128 + signal
            } else {
                let code = job.exit_code.unwrap_or(125);
                if (0..=255).contains(&code) {
                    code as i32
                } else {
                    eprintln!("[xrun] remote exit code {code}");
                    1
                }
            }
        }
    }
}
fn show_job(json: bool, job: &Job) {
    print(json, job, || {
        println!("{}\t{:?}\t{}", job_ref(job), job.state, job.program)
    });
}
fn read_input(max: u64) -> Result<Vec<u8>> {
    let mut bytes = vec![];
    std::io::stdin().take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!(ErrorCode::InputTooLarge.error(format!("maximum {max} bytes")))
    }
    Ok(bytes)
}

fn save_download(
    bytes: &[u8],
    path: Option<PathBuf>,
    prefix: &str,
    suffix: &str,
) -> Result<PathBuf> {
    if let Some(path) = path {
        return crate::transfer::save_local(&std::env::current_dir()?.join(path), bytes);
    }
    let mut temp = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(suffix)
        .tempfile()?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    config::sync_parent(temp.path())?;
    let (_, path) = temp.keep()?;
    Ok(path)
}
async fn remote(cli: DeviceCli) -> Result<i32> {
    let json = cli.json;
    if matches!(&cli.command, Remote::Start(e) if e.interactive)
        || (json && matches!(&cli.command, Remote::Run(e) | Remote::Start(e) if e.interactive))
    {
        eprintln!("[xrun] -i cannot be combined with start or --json");
        return Ok(2);
    }
    if let Remote::Jobs {
        id: Some(_),
        running,
        request_id,
        ..
    } = &cli.command
        && (*running || request_id.is_some())
    {
        eprintln!("[xrun] jobs <ID> cannot be combined with list filters");
        return Ok(2);
    }
    if matches!(&cli.command,Remote::Pull{local:Some(local),..}if local=="-"&&json) {
        eprintln!("[xrun] pull to stdout cannot be combined with --json");
        return Ok(2);
    }
    let id = identity().await?;
    let explicit = match &cli.command {
        Remote::Run(e) | Remote::Start(e) => e.request_id.as_deref(),
        _ => None,
    };
    let submissions = if matches!(&cli.command, Remote::Run(e) | Remote::Start(e) if !e.interactive)
    {
        Some(SubmissionStore::open(
            &config::device_dir()?.join("submissions.sqlite"),
        )?)
    } else {
        None
    };
    let prior = explicit
        .map(|r| submissions.as_ref().context("submission store")?.get(r))
        .transpose()?
        .flatten();
    if prior
        .as_ref()
        .is_some_and(|p| cli.device != p.target_device_id && cli.device != p.target_name)
    {
        diagnostic(
            json,
            &anyhow::anyhow!(
                ErrorCode::DeviceMismatch.error("request-id belongs to another target device")
            ),
        );
        return Ok(2);
    }
    if prior.as_ref().is_some_and(|s| {
        s.source_device_id != id.device_id
            || crypto::ca_spki_pin(&id.ca_pem).ok().as_deref() != Some(&s.ca_pin)
    }) {
        bail!(
            ErrorCode::IdentityMismatch
                .error("submission belongs to another identity or deployment")
        )
    }
    let selected = prior
        .as_ref()
        .map(|s| s.target_device_id.as_str())
        .unwrap_or(&cli.device);
    // The operation session authenticates the target and exchanges the latest
    // signed roster. Only info needs a separate live state query.
    let roster = crate::network::current(&id)?;
    let target = roster.member(selected)?;
    if target.revoked {
        bail!(ErrorCode::DeviceRevoked.error("target has been revoked"))
    }
    let target_name = target.name.clone();
    let target = &target.device_id;
    match cli.command {
        Remote::Run(e) if e.interactive => {
            let mut s = session(&id, target).await?;
            let execution = StreamExecution {
                program: e
                    .command
                    .first()
                    .context(ErrorCode::InvalidRequest.error("program required"))?
                    .clone(),
                args: e.command.into_iter().skip(1).collect(),
                cwd: e.cwd.unwrap_or(s.cwd),
                env: e.env.into_iter().collect(),
                timeout: e.timeout.unwrap_or(1800),
            };
            net::send(
                &mut s.ws,
                &Data::Request {
                    request: Request::StreamExec { execution },
                },
            )
            .await?;
            if !matches!(response(&mut s.ws).await?, Data::StreamReady) {
                bail!(ErrorCode::InvalidMessage.error("expected stream acknowledgement"))
            }
            let result = tokio::select! {
                result = crate::streaming::client(&mut s.ws) => result?,
                _ = tokio::signal::ctrl_c() => return Ok(130),
                _ = termination() => return Ok(125),
            };
            Ok(if result.timed_out {
                124
            } else if let Some(signal) = result.signal {
                128 + signal
            } else {
                match result.exit_code {
                    Some(code) if (0..=255).contains(&code) => code as i32,
                    Some(code) => {
                        eprintln!("[xrun] remote exit code {code}");
                        1
                    }
                    None => 125,
                }
            })
        }
        Remote::Run(e) => {
            Box::pin(execute_cli(
                &id,
                (target, &target_name),
                e,
                false,
                json,
                submissions.as_ref().context("submission store")?,
                prior,
            ))
            .await
        }
        Remote::Start(e) => {
            Box::pin(execute_cli(
                &id,
                (target, &target_name),
                e,
                true,
                json,
                submissions.as_ref().context("submission store")?,
                prior,
            ))
            .await
        }
        Remote::Info => {
            let target_metadata: Device = net::http(
                &id,
                reqwest::Method::GET,
                &format!("/devices/{target}"),
                None,
            )
            .await?;
            print(json, &target_metadata, || {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&target_metadata).unwrap()
                )
            });
            Ok(0)
        }
        Remote::Forward { ports } => forward_cli(id, target, ports, json).await,
        Remote::Jobs {
            id: job,
            running,
            request_id,
            limit,
            offset,
        } => {
            let job = match job {
                Some(value) => match parse_job(&value, target, &target_name) {
                    Ok(id) => Some(id),
                    Err(e) => {
                        diagnostic(json, &e);
                        return Ok(2);
                    }
                },
                None => None,
            };
            let value = request(
                &id,
                target,
                Request::Jobs {
                    id: job,
                    running,
                    request_id,
                    limit,
                    offset,
                },
            )
            .await?;
            match value {
                Data::Job { job } => show_job(json, &job),
                Data::Jobs { jobs } => print(json, &jobs, || {
                    for j in &jobs {
                        show_job(false, j)
                    }
                }),
                _ => bail!(ErrorCode::InvalidMessage.error("expected jobs")),
            };
            Ok(0)
        }
        Remote::Wait {
            id: job,
            timeout,
            tail,
        } => {
            let job = match parse_job(&job, target, &target_name) {
                Ok(j) => j,
                Err(e) => {
                    diagnostic(json, &e);
                    return Ok(2);
                }
            };
            let result = if timeout == 0 {
                wait(&id, target, &job).await
            } else {
                match tokio::time::timeout(Duration::from_secs(timeout), wait(&id, target, &job))
                    .await
                {
                    Ok(r) => r,
                    Err(_) => {
                        diagnostic(
                            json,
                            &anyhow::anyhow!(
                                ErrorCode::WaitTimeout.error("task continues running")
                            ),
                        );
                        return Ok(75);
                    }
                }
            };
            match result {
                Ok(job) => {
                    let (logs, logs_error) = if tail == 0 {
                        (vec![], None)
                    } else {
                        match collect_logs(&id, target, &job.job_id, 0).await {
                            Ok((logs, state)) => (logs, log_error(&state)),
                            Err(e) => (vec![], Some(e.to_string())),
                        }
                    };
                    let logs = tail_events(logs, tail);
                    if json {
                        println!(
                            "{}",
                            serde_json::json!({"job":job,"logs":logs,"logs_error":logs_error})
                        );
                    } else {
                        output(&logs)?;
                        if let Some(e) = &logs_error {
                            eprintln!("[xrun] {e}")
                        }
                        if let Some(e) = job.error.as_ref().or(job.incomplete_reason.as_ref()) {
                            eprintln!("[xrun] {e}")
                        }
                    }
                    Ok(job_code(&job))
                }
                Err(e) => {
                    diagnostic(json, &e);
                    Ok(
                        if net::explicit(&e) && !crate::error::is(&e, ErrorCode::DeviceOffline) {
                            125
                        } else {
                            75
                        },
                    )
                }
            }
        }
        Remote::Logs {
            id: job,
            follow,
            after,
            tail,
        } => {
            let job = match parse_job(&job, target, &target_name) {
                Ok(j) => j,
                Err(e) => {
                    diagnostic(json, &e);
                    return Ok(2);
                }
            };
            if let Some(tail) = tail {
                let (all, state) = collect_logs(&id, target, &job, after).await?;
                let latest = all.last().map(|e| e.seq).unwrap_or(after);
                let logs = tail_events(all, tail);
                if json {
                    println!("{}", serde_json::to_string(&logs)?)
                } else {
                    output(&logs)?
                }
                if !follow {
                    return Ok(log_code(&state, json));
                }
                let mut cursor = LogCursor {
                    after: latest,
                    db_id: Some(state.db_id),
                    received: false,
                };
                let state = stream_logs(&id, target, &job, true, json, &mut cursor).await?;
                return Ok(log_code(&state, json));
            }
            let mut cursor = LogCursor {
                after,
                db_id: None,
                received: false,
            };
            let state = stream_logs(&id, target, &job, follow, json, &mut cursor).await?;
            Ok(log_code(&state, json))
        }
        Remote::Kill { id: job } => {
            let job = match parse_job(&job, target, &target_name) {
                Ok(j) => j,
                Err(e) => {
                    diagnostic(json, &e);
                    return Ok(2);
                }
            };
            let mut s = session(&id, target).await?;
            let operation = async {
                net::send(
                    &mut s.ws,
                    &Data::Request {
                        request: Request::Kill { id: job },
                    },
                )
                .await?;
                response(&mut s.ws).await
            };
            let result = match tokio::time::timeout(Duration::from_secs(10), operation).await {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    ErrorCode::Unconfirmed.error("cancellation response timed out")
                )),
            };
            match result {
                Ok(Data::Job { job }) => {
                    s.finish().await;
                    show_job(json, &job);
                    Ok(0)
                }
                Ok(_) => bail!(ErrorCode::InvalidMessage.error("expected job")),
                Err(e) => {
                    diagnostic(json, &e);
                    Ok(if definitive(&e) { 125 } else { 75 })
                }
            }
        }
        Remote::Push {
            local,
            remote,
            cwd,
            mkdir,
            no_overwrite,
            expect,
        } => {
            let input_snapshot;
            let (file, size, hash) = if local == "-" {
                let (temp, size, hash) = crate::transfer::snapshot_input(std::io::stdin().lock())?;
                input_snapshot = temp;
                (input_snapshot.reopen()?, size, hash)
            } else {
                crate::transfer::prepare_upload(Path::new(&local))?
            };
            let mut s = session(&id, target).await?;
            let path = remote;
            let sent = std::sync::atomic::AtomicBool::new(false);
            let operation = async {
                sent.store(true, std::sync::atomic::Ordering::SeqCst);
                net::send(
                    &mut s.ws,
                    &Data::Request {
                        request: Request::Push {
                            path,
                            cwd,
                            size,
                            sha256: hash.clone(),
                            mkdir,
                            no_overwrite,
                            expect,
                        },
                    },
                )
                .await?;
                net::send_file(&mut s.ws, &file).await?;
                response(&mut s.ws).await
            };
            let result = tokio::select! {
                r=operation=>r,
                _=tokio::signal::ctrl_c()=>{return Ok(if sent.load(std::sync::atomic::Ordering::SeqCst){diagnostic(json,&anyhow::anyhow!(ErrorCode::Unconfirmed.error("upload interrupted; pull the destination before retrying")));75}else{130});},
                _=termination()=>{return Ok(if sent.load(std::sync::atomic::Ordering::SeqCst){75}else{125});},
            };
            match result {
                Ok(Data::File { path, .. }) => {
                    s.finish().await;
                    print(
                        json,
                        &serde_json::json!({"device_id":target,"remote_path":path,"size":size,"sha256":hash}),
                        || println!("{path}"),
                    );
                    Ok(0)
                }
                Ok(_) => bail!(ErrorCode::InvalidMessage.error("expected file confirmation")),
                Err(e) => {
                    diagnostic(json, &e);
                    Ok(
                        if net::explicit(&e) || crate::error::is(&e, ErrorCode::DeviceBusy) {
                            125
                        } else if definitive(&e) {
                            1
                        } else {
                            75
                        },
                    )
                }
            }
        }
        Remote::Pull { remote, local, cwd } => {
            let mut s = session(&id, target).await?;
            let path = remote.clone();
            net::send(
                &mut s.ws,
                &Data::Request {
                    request: Request::Pull { path, cwd },
                },
            )
            .await?;
            let Data::File {
                path, size, sha256, ..
            } = response(&mut s.ws).await?
            else {
                bail!(ErrorCode::InvalidMessage.error("expected file header"))
            };
            let mut temp =
                net::receive_file_with_prefix(&mut s.ws, size, &sha256, "xrun-pull-").await?;
            s.finish().await;
            if local.as_deref() == Some("-") {
                std::io::copy(temp.as_file_mut(), &mut std::io::stdout().lock())?;
                return Ok(0);
            }
            let local = if let Some(path) = local {
                crate::transfer::save_local_reader(
                    &std::env::current_dir()?.join(path),
                    temp.as_file_mut(),
                )?
            } else {
                temp.as_file().sync_all()?;
                config::sync_parent(temp.path())?;
                temp.keep()?.1
            };
            print(
                json,
                &serde_json::json!({"path":local,"device_id":target,"remote_path":path,"size":size,"sha256":sha256}),
                || println!("{}", local.display()),
            );
            Ok(0)
        }
        Remote::Screenshot { local } => {
            let mut s = session(&id, target).await?;
            net::send(
                &mut s.ws,
                &Data::Request {
                    request: Request::Screenshot,
                },
            )
            .await?;
            let Data::File {
                size,
                sha256,
                width,
                height,
                captured_at,
                ..
            } = response(&mut s.ws).await?
            else {
                bail!(ErrorCode::InvalidMessage.error("expected screenshot header"))
            };
            let bytes = net::receive_bytes(&mut s.ws, size, &sha256, MAX_FILE).await?;
            s.finish().await;
            let path = save_download(&bytes, local, "xrun-screen-", ".png")?;
            print(
                json,
                &serde_json::json!({"path":path,"device_id":target,"width":width,"height":height,"captured_at":captured_at}),
                || println!("{}", path.display()),
            );
            Ok(0)
        }
    }
}
async fn forward_cli(id: Identity, target: &str, ports: (u16, u16), json: bool) -> Result<i32> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, ports.0))
        .await
        .context(ErrorCode::ForwardListenFailed.error("cannot bind local loopback port"))?;
    let address = listener.local_addr()?;
    print(
        json,
        &serde_json::json!({"device_id":target,"local_address":address.to_string(),"remote_port":ports.1}),
        || println!("{address} -> {target}:{} (Ctrl-C to stop)", ports.1),
    );
    let mut connections = tokio::task::JoinSet::new();
    let target = target.to_owned();
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (tcp, _) = result?;
                if connections.len() >= 32 {
                    diagnostic(json, &anyhow::anyhow!(ErrorCode::DeviceBusy.error("too many local forwarded connections")));
                    continue;
                }
                let id = id.clone();
                let target = target.clone();
                connections.spawn(async move {
                    let mut session = session(&id, &target).await?;
                    net::send(&mut session.ws, &Data::Request { request: Request::Forward { port: ports.1 } }).await?;
                    match tokio::time::timeout(Duration::from_secs(10), response(&mut session.ws)).await
                        .context(ErrorCode::ForwardTimeout.error("target did not acknowledge the connection"))?? {
                        Data::ForwardReady { port } if port == ports.1 => crate::forwarding::bridge(&mut session.ws, tcp).await,
                        _ => bail!(ErrorCode::InvalidMessage.error("expected forwarding acknowledgement")),
                    }
                });
            },
            result = connections.join_next(), if !connections.is_empty() => {
                match result {
                    Some(Ok(Ok(()))) | None => {},
                    Some(Ok(Err(error))) => diagnostic(json, &error),
                    Some(Err(error)) => diagnostic(json, &anyhow::anyhow!(error)),
                }
            },
            _ = tokio::signal::ctrl_c() => break,
            _ = termination() => break,
        }
    }
    connections.shutdown().await;
    Ok(0)
}

fn network_error(error: &anyhow::Error) -> bool {
    crate::error::code(error).is_some_and(|code| code.is_network())
        || error.chain().any(|e| {
            e.downcast_ref::<reqwest::Error>()
                .is_some_and(|e| e.is_connect() || e.is_timeout())
                || e.downcast_ref::<tokio_tungstenite::tungstenite::Error>()
                    .is_some()
        })
}
fn definitive(error: &anyhow::Error) -> bool {
    crate::error::code(error).is_some_and(|code| code.rejects_submission())
}
async fn execute_cli(
    id: &Identity,
    selected: (&str, &str),
    e: Execute,
    background: bool,
    json: bool,
    store: &SubmissionStore,
    prior: Option<Submission>,
) -> Result<i32> {
    let (target, target_name) = selected;
    let input = if e.stdin || e.script.is_some() {
        read_input(MAX_INPUT as u64)?
    } else {
        vec![]
    };
    if e.script
        .as_ref()
        .is_some_and(|s| !["sh", "bash", "zsh", "powershell", "pwsh", "cmd"].contains(&s.as_str()))
    {
        diagnostic(
            json,
            &anyhow::anyhow!(
                ErrorCode::InvalidShell.error("select sh, bash, zsh, powershell, pwsh or cmd")
            ),
        );
        return Ok(2);
    }
    let mut s = session(id, target).await?;
    let cwd = e.cwd.unwrap_or(s.cwd.clone());
    // Remote Windows paths must be validated by the target, not the source OS.
    let program = if e.script.is_some() {
        String::new()
    } else {
        e.command[0].clone()
    };
    let args = if e.script.is_some() {
        e.command
    } else {
        e.command[1..].to_vec()
    };
    let execution = Execution {
        request_id: e
            .request_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        db_id: prior
            .as_ref()
            .map(|p| p.db_id.clone())
            .unwrap_or(s.db_id.clone()),
        program,
        args,
        cwd,
        env: e.env.into_iter().collect::<BTreeMap<_, _>>(),
        timeout: e.timeout.unwrap_or(if background { 0 } else { 1800 }),
        shell: e.script,
        input_size: input.len() as u64,
        input_sha256: sha256(&input),
    };
    let hash = execution.hash();
    if let Some(prior) = &prior {
        if prior.request_hash != hash {
            diagnostic(
                json,
                &anyhow::anyhow!(
                    ErrorCode::RequestConflict.error("original execution parameters must match")
                ),
            );
            return Ok(2);
        }
        if prior.db_id != s.db_id {
            diagnostic(
                json,
                &anyhow::anyhow!(
                    ErrorCode::DbReset.error("original task database no longer exists")
                ),
            );
            return Ok(125);
        }
    }
    let mut submission = prior.unwrap_or(Submission {
        request_id: execution.request_id.clone(),
        source_device_id: id.device_id.clone(),
        target_device_id: target.into(),
        target_name: target_name.into(),
        ca_pin: crypto::ca_spki_pin(&id.ca_pem)?,
        db_id: execution.db_id.clone(),
        request_hash: hash,
        program: execution.program.clone(),
        created_at_ms: now_ms(),
        job_id: None,
        status: "not_accepted".into(),
    });
    let header = Data::Request {
        request: Request::Exec {
            execution: execution.clone(),
            follow: !background,
        },
    };
    if serde_json::to_vec(&header)?.len() > MAX_MESSAGE {
        diagnostic(
            json,
            &anyhow::anyhow!(ErrorCode::InvalidCommand.error("execution header exceeds 1 MiB")),
        );
        return Ok(2);
    }
    store.save(&submission)?;
    submission.status = "unconfirmed".into();
    store.save(&submission)?;
    let sent = std::sync::atomic::AtomicBool::new(false);
    let submitted = async {
        sent.store(true, std::sync::atomic::Ordering::SeqCst);
        net::send(
            &mut s.ws,
            &Data::Request {
                request: Request::Exec {
                    execution: execution.clone(),
                    follow: !background,
                },
            },
        )
        .await?;
        net::send_bytes(&mut s.ws, &input).await?;
        match response(&mut s.ws).await? {
            Data::Job { job } => Ok(job),
            _ => bail!(ErrorCode::InvalidMessage.error("expected job acknowledgement")),
        }
    };
    let (job, acknowledged) = tokio::select! {
        r=submitted=>match r{Ok(job)=>(job, true),Err(error)=>{
            if definitive(&error){submission.status="not_accepted".into();store.save(&submission)?;diagnostic(json,&error);return Ok(125)}
            match recover(id,target,&execution.request_id,Some(&execution.db_id)).await{Ok(job)=>(job, false),Err(e)=>{if crate::error::is(&e, ErrorCode::DbReset){diagnostic(json,&e);return Ok(125)}diagnostic(json,&anyhow::anyhow!(ErrorCode::Unconfirmed.error(format!("request {} may have executed; use recent or jobs --request-id",execution.request_id))));return Ok(75)}}
        }},
        _=tokio::signal::ctrl_c()=>{if !sent.load(std::sync::atomic::Ordering::SeqCst){submission.status="not_accepted".into();store.save(&submission)?;return Ok(130)}return Box::pin(cancel_unknown(id,target,&execution.request_id,&execution.db_id,json)).await;},
        _=termination()=>{if !sent.load(std::sync::atomic::Ordering::SeqCst){submission.status="not_accepted".into();store.save(&submission)?;return Ok(125)}diagnostic(json,&anyhow::anyhow!(ErrorCode::Unconfirmed.error(format!("request {} may have executed",execution.request_id))));return Ok(75)}
    };
    submission.job_id = Some(job_ref(&job));
    submission.status = "confirmed".into();
    store.save(&submission)?;
    if background {
        if acknowledged {
            s.finish().await;
        }
        print(json, &job, || println!("{target_name}/{}", job.job_id));
        return Ok(0);
    }
    let mut cursor = LogCursor {
        after: 0,
        db_id: Some(job.db_id.clone()),
        received: false,
    };
    let mut initial = acknowledged.then_some(s);
    let mut deadline = None;
    loop {
        let result = {
            let logs = async {
                if let Some(mut session) = initial.take() {
                    let job = receive_logs(&mut session.ws, false, &mut cursor).await?;
                    session.finish().await;
                    Ok(job)
                } else {
                    stream_logs(id, target, &job.job_id, true, false, &mut cursor).await
                }
            };
            tokio::pin!(logs);
            tokio::select! {
                r=&mut logs=>r,
                _=tokio::signal::ctrl_c()=>{return Box::pin(cancel_known(id,target,&job.job_id,json)).await},
                _=termination()=>{diagnostic(json,&anyhow::anyhow!(ErrorCode::Unconfirmed.error(format!("{} continues remotely",job_ref(&job)))));return Ok(75)}
            }
        };
        match result {
            Ok(result) => {
                if let Some(error) = result.error.as_ref().or(result.incomplete_reason.as_ref()) {
                    eprintln!("[xrun] {error}")
                }
                return Ok(job_code(&result));
            }
            Err(error) => {
                if crate::error::is(&error, ErrorCode::DbReset) {
                    diagnostic(json, &error);
                    return Ok(125);
                }
                if cursor.received {
                    deadline = None;
                    cursor.received = false;
                }
                let end =
                    *deadline.get_or_insert(tokio::time::Instant::now() + Duration::from_secs(30));
                if tokio::time::Instant::now() >= end {
                    diagnostic(
                        json,
                        &anyhow::anyhow!(
                            ErrorCode::Unconfirmed.error(format!(
                                "result unavailable for {}: {error}",
                                job_ref(&job)
                            ))
                        ),
                    );
                    return Ok(75);
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}
async fn termination() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("TERM handler");
        let mut hup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .expect("HUP handler");
        tokio::select! {_=term.recv()=>{},_=hup.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        std::future::pending::<()>().await;
    }
}
async fn recover(
    id: &Identity,
    target: &str,
    request_id: &str,
    expected_db: Option<&str>,
) -> Result<Job> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match session(id, target).await {
                Ok(mut s) => {
                    if expected_db.is_some_and(|db| db != s.db_id) {
                        bail!(ErrorCode::DbReset.error("original task database no longer exists"))
                    }
                    net::send(
                        &mut s.ws,
                        &Data::Request {
                            request: Request::Jobs {
                                id: None,
                                running: false,
                                request_id: Some(request_id.into()),
                                limit: 1,
                                offset: 0,
                            },
                        },
                    )
                    .await?;
                    if let Data::Jobs { jobs } = response(&mut s.ws).await? {
                        s.finish().await;
                        if let Some(job) = jobs.into_iter().next() {
                            return Ok(job);
                        }
                    }
                }
                Err(e) if net::explicit(&e) && !crate::error::is(&e, ErrorCode::DeviceOffline) => {
                    return Err(e);
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .context(ErrorCode::Unconfirmed.error("request recovery timed out"))?
}
async fn cancel_unknown(
    id: &Identity,
    target: &str,
    request_id: &str,
    db_id: &str,
    json: bool,
) -> Result<i32> {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let job = recover(id, target, request_id, Some(db_id)).await?;
        cancel_known(id, target, &job.job_id, json).await
    })
    .await;
    match result {
        Ok(Ok(code)) => Ok(code),
        _ => {
            diagnostic(
                json,
                &anyhow::anyhow!(
                    ErrorCode::Unconfirmed
                        .error(format!("cancellation unconfirmed for request {request_id}"))
                ),
            );
            Ok(75)
        }
    }
}
async fn cancel_known(id: &Identity, target: &str, job: &str, json: bool) -> Result<i32> {
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        request(id, target, Request::Kill { id: job.into() }).await?;
        wait(id, target, job).await
    })
    .await;
    match result {
        Ok(Ok(job)) => {
            if json {
                show_job(true, &job)
            }
            Ok(if job.state == JobState::Canceled {
                130
            } else {
                job_code(&job)
            })
        }
        _ => {
            diagnostic(
                json,
                &anyhow::anyhow!(
                    ErrorCode::Unconfirmed
                        .error(format!("cancellation unconfirmed for {target}/{job}"))
                ),
            );
            Ok(75)
        }
    }
}
async fn wait(id: &Identity, target: &str, job: &str) -> Result<Job> {
    match request(id, target, Request::Wait { id: job.into() }).await? {
        Data::Job { job } => Ok(job),
        _ => bail!(ErrorCode::InvalidMessage.error("expected final job state")),
    }
}
fn output(events: &[LogEvent]) -> Result<()> {
    let mut out = std::io::stdout().lock();
    let mut err = std::io::stderr().lock();
    for e in events {
        let bytes = STANDARD.decode(&e.data_base64)?;
        if e.stream == "stderr" {
            err.write_all(&bytes)?;
            err.flush()?
        } else {
            out.write_all(&bytes)?;
            out.flush()?
        }
    }
    Ok(())
}
struct LogCursor {
    after: u64,
    db_id: Option<String>,
    received: bool,
}
fn log_error(job: &Job) -> Option<String> {
    job.incomplete_reason.as_ref().map(|reason| {
        let code = match reason.as_str() {
            "TRUNCATED" => "LOG_TRUNCATED",
            "LOG_EXPIRED" => "LOG_UNAVAILABLE",
            _ => "LOG_INCOMPLETE",
        };
        format!("{code}: {reason}")
    })
}
fn log_code(job: &Job, json: bool) -> i32 {
    if let Some(error) = log_error(job) {
        diagnostic(json, &anyhow::anyhow!(error));
        1
    } else {
        0
    }
}
async fn stream_logs(
    id: &Identity,
    target: &str,
    job: &str,
    follow: bool,
    json: bool,
    cursor: &mut LogCursor,
) -> Result<Job> {
    let mut s = session(id, target).await?;
    if cursor.db_id.as_ref().is_some_and(|db| *db != s.db_id) {
        bail!(ErrorCode::DbReset.error("original task database no longer exists"))
    }
    cursor.db_id = Some(s.db_id.clone());
    net::send(
        &mut s.ws,
        &Data::Request {
            request: Request::Logs {
                id: job.into(),
                after: cursor.after,
                follow,
            },
        },
    )
    .await?;
    let job = receive_logs(&mut s.ws, json, cursor).await?;
    s.finish().await;
    Ok(job)
}
async fn receive_logs(ws: &mut Ws, json: bool, cursor: &mut LogCursor) -> Result<Job> {
    let mut final_job = None;
    loop {
        match response(ws).await? {
            Data::Logs { events, job } => {
                cursor.received = true;
                if json {
                    if !events.is_empty() {
                        println!("{}", serde_json::to_string(&events)?);
                    }
                } else {
                    output(&events)?
                };
                if let Some(e) = events.last() {
                    cursor.after = e.seq
                }
                final_job = Some(job)
            }
            Data::End => {
                return final_job
                    .context(ErrorCode::InvalidMessage.error("logs ended without job state"));
            }
            _ => bail!(ErrorCode::InvalidMessage.error("expected logs")),
        }
    }
}
async fn collect_logs(
    id: &Identity,
    target: &str,
    job: &str,
    after: u64,
) -> Result<(Vec<LogEvent>, Job)> {
    let mut s = session(id, target).await?;
    net::send(
        &mut s.ws,
        &Data::Request {
            request: Request::Logs {
                id: job.into(),
                after,
                follow: false,
            },
        },
    )
    .await?;
    let mut events = vec![];
    let mut state = None;
    loop {
        match response(&mut s.ws).await? {
            Data::Logs { events: chunk, job } => {
                events.extend(chunk);
                state = Some(job);
            }
            Data::End => {
                s.finish().await;
                return Ok((
                    events,
                    state
                        .context(ErrorCode::InvalidMessage.error("logs ended without job state"))?,
                ));
            }
            _ => bail!(ErrorCode::InvalidMessage.error("expected logs")),
        }
    }
}
fn tail_events(events: Vec<LogEvent>, lines: usize) -> Vec<LogEvent> {
    if lines == 0 {
        return vec![];
    }
    let mut remaining = lines;
    let mut first = true;
    let mut result = vec![];
    for mut event in events.into_iter().rev() {
        let Ok(bytes) = STANDARD.decode(&event.data_base64) else {
            continue;
        };
        let mut start = 0;
        let mut done = false;
        for i in (0..bytes.len()).rev() {
            let skip = first && bytes[i] == b'\n';
            first = false;
            if bytes[i] == b'\n' && !skip {
                if remaining <= 1 {
                    start = i + 1;
                    done = true;
                    break;
                }
                remaining -= 1;
            }
        }
        event.data_base64 = STANDARD.encode(&bytes[start..]);
        result.push(event);
        if done {
            break;
        }
    }
    result.reverse();
    result
}
