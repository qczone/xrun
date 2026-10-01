use crate::{
    config::{self, Identity, PendingIdentity, ServerConfig},
    crypto, daemon,
    net::{self, Ws},
    protocol::*,
    service,
    store::{Invitation, ServerStore, Submission, SubmissionStore},
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
    after_help = "Remote: xrun <device> [options] -- <program> [args]\n        xrun <device> start|info|jobs|wait|logs|kill|push|pull|screenshot\nUse xrun guide for examples."
)]
struct LocalCli {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Local,
}
#[derive(Subcommand)]
enum Local {
    /// Deploy the Server on Linux and install local services
    Up {
        #[arg(long)]
        port: Option<u16>,
        #[arg(long,action=clap::ArgAction::Append,value_delimiter=',')]
        addr: Vec<String>,
        #[arg(long)]
        no_detect: bool,
        #[arg(long)]
        no_service: bool,
        #[arg(long)]
        no_daemon: bool,
        /// Grant mutual access to the joining device
        #[arg(long)]
        allow: bool,
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
    AllowFrom { device: String },
    /// Deny a source device access to this machine
    DenyFrom { device: String },
    /// Revoke a device identity (admin only)
    Revoke { device: String },
    /// Show local state and the deployment's devices
    Status,
    /// Show this CLI's submissions from the last 24 hours
    Recent,
    /// Remove local services; optionally purge local data
    Down {
        #[arg(long)]
        purge: bool,
    },
    /// Run the Server in the foreground (Linux only)
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
enum DaemonCommand {
    Install,
    Uninstall,
    Reset,
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
        bail!("UNSUPPORTED_PLATFORM: Server deployment requires Linux")
    }
    Ok(())
}
fn user_name() -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| std::env::consts::OS.into());
    let mut value: String = host
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    if value.is_empty() || !value.as_bytes()[0].is_ascii_lowercase() || !valid_name(&value) {
        value = format!("{}1", std::env::consts::OS)
    }
    value
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
            port,
            addr,
            no_detect,
            no_service,
            no_daemon,
            allow,
        } => up(port, addr, no_detect, no_service, no_daemon, allow, json).await?,
        Local::Join {
            link,
            name,
            no_daemon,
        } => {
            let id = join(&link, name).await?;
            daemon::init()?;
            if !no_daemon {
                service::install("daemon").await?
            }
            print(
                json,
                &serde_json::json!({"device_id":id.device_id,"name":id.name}),
                || println!("{} ({})", id.name, id.device_id),
            );
            if id.registration.allow_inviter
                && let Some(inviter) = &id.registration.inviter_id
            {
                let info = net::http::<Device>(
                    &id,
                    reqwest::Method::GET,
                    &format!("/devices/{inviter}"),
                    None,
                )
                .await;
                if info.as_ref().is_ok_and(|device| !device.online) {
                    diagnostic(
                        json,
                        &anyhow::anyhow!(
                            "INVITER_OFFLINE: on the inviting device, run xrun allow-from {} if not already allowed",
                            id.device_id
                        ),
                    );
                }
            }
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
                bail!("VERSION_MISMATCH: invitation policy differs; upgrade all components")
            }
            print(json, &value, || {
                println!("{}", value["link"].as_str().unwrap_or_default())
            });
            invitation_notice(allow);
        }
        Local::AllowFrom { device } => permission(&device, true, json).await?,
        Local::DenyFrom { device } => permission(&device, false, json).await?,
        Local::Revoke { device } => {
            let id = identity().await?;
            let v: serde_json::Value = net::http(
                &id,
                reqwest::Method::POST,
                "/admin/revoke",
                Some(serde_json::json!({"device":device})),
            )
            .await?;
            print(json, &v, || println!("revoked {}", v["device_id"]));
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
                    bail!("INTERACTIVE_REQUIRED: --purge requires terminal confirmation")
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
        Local::Server => {
            require_linux()?;
            let mut cfg = ServerConfig::load()?;
            if !cfg.manual {
                cfg.addresses = detect_addresses(cfg.port, cfg.no_detect).await?;
                cfg.save()?;
            }
            server_foreground(cfg).await?
        }
        Local::Daemon { operation } => match operation {
            None => daemon::run().await?,
            Some(DaemonCommand::Install) => {
                daemon::init()?;
                service::install("daemon").await?
            }
            Some(DaemonCommand::Uninstall) => service::uninstall("daemon").await?,
            Some(DaemonCommand::Reset) => daemon::reset()?,
        },
        Local::Guide => print!("{}", include_str!("../README.md")),
    }
    Ok(0)
}
async fn status(json: bool) -> Result<i32> {
    let dir = config::device_dir()?;
    let mut id = if dir.join("identity.toml").exists() {
        Some(Identity::load()?)
    } else {
        None
    };
    let mut error = None;
    let mut devices = None;
    if let Some(identity) = &mut id {
        match net::renew_identity(identity).await {
            Ok(()) => {
                match net::http::<Vec<Device>>(identity, reqwest::Method::GET, "/devices", None)
                    .await
                {
                    Ok(values) => devices = Some(values),
                    Err(e) => error = Some(e),
                }
            }
            Err(e) => error = Some(e),
        }
    }
    let daemon_running = config::instance_running(&dir.join("daemon.lock"))?;
    let server_running = if let Ok(cfg) = ServerConfig::load() {
        config::instance_running(&cfg.data_dir.join("server.lock"))?
    } else {
        false
    };
    let local = serde_json::json!({"joined":id.is_some(),"device_id":id.as_ref().map(|i|&i.device_id),"name":id.as_ref().map(|i|&i.name),"version":VERSION,"daemon_initialized":dir.join("daemon.initialized").exists(),"daemon_installed":service::installed("daemon")?,"daemon_running":daemon_running,"server_configured":dir.join("config.toml").exists(),"server_installed":service::installed("server")?,"server_running":server_running});
    print(
        json,
        &serde_json::json!({"local":local,"devices":devices,"server_error":error.as_ref().map(Data::error)}),
        || {
            if let Some(id) = &id {
                println!("local: {} ({})", id.name, id.device_id);
            } else {
                println!("local: not joined");
            }
            println!(
                "daemon: {}",
                if daemon_running { "running" } else { "stopped" }
            );
            if local["server_configured"] == true {
                println!(
                    "server: {}",
                    if server_running { "running" } else { "stopped" }
                );
            }
            if let Some(devices) = &devices {
                for d in devices {
                    println!(
                        "{}\t{}\t{}",
                        d.name,
                        d.device_id,
                        if d.revoked {
                            "revoked"
                        } else if d.online {
                            "online"
                        } else {
                            "offline"
                        }
                    );
                }
            }
            if let Some(error) = &error {
                eprintln!("[xrun] device list unavailable: {error:#}");
            }
        },
    );
    Ok(if error.is_some() { 125 } else { 0 })
}
async fn permission(value: &str, allow: bool, json: bool) -> Result<()> {
    let id = Identity::load()?;
    let device_id = if let Some(key) = value.strip_prefix("dev_") {
        if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("INVALID_DEVICE_ID: expected dev_ followed by 32 hexadecimal digits");
        }
        value.to_string()
    } else {
        let device: Device = net::http(
            &id,
            reqwest::Method::GET,
            &format!("/devices/{value}"),
            None,
        )
        .await?;
        device.device_id
    };
    config::update_permission(&device_id, allow)?;
    print(
        json,
        &serde_json::json!({"source_device_id":device_id,"allowed":allow}),
        || println!("{} {}", if allow { "allowed" } else { "denied" }, device_id),
    );
    Ok(())
}
fn parse_link(link: &str) -> Result<(Vec<String>, String, String)> {
    let rest = link
        .strip_prefix("xrun://")
        .context("INVALID_LINK: expected xrun://")?;
    let (rest, token) = rest
        .split_once('#')
        .context("INVALID_LINK: missing token")?;
    let (addresses, pin) = rest
        .split_once('/')
        .context("INVALID_LINK: missing CA pin")?;
    if pin.len() != 52
        || !pin
            .bytes()
            .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
        || token.len() != 26
        || !token
            .bytes()
            .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
    {
        bail!("INVALID_LINK: invalid CA pin or invitation token")
    }
    let mut urls = vec![];
    for address in addresses.split(',') {
        validate_address(address)?;
        urls.push(format!("https://{address}"))
    }
    if urls.is_empty() {
        bail!("INVALID_LINK: missing addresses")
    }
    Ok((urls, pin.into(), token.into()))
}
fn validate_address(address: &str) -> Result<()> {
    let (_, port) = address
        .rsplit_once(':')
        .context("INVALID_ADDRESS: an explicit port is required")?;
    if port.parse::<u16>().ok().is_none_or(|p| p == 0) {
        bail!("INVALID_ADDRESS: invalid port")
    }
    let url = url::Url::parse(&format!("https://{address}"))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || matches!(url.host(), Some(url::Host::Ipv6(_)))
    {
        bail!("INVALID_ADDRESS: expected IPv4 or hostname followed by :port")
    };
    Ok(())
}
async fn join(link: &str, name: Option<String>) -> Result<Identity> {
    let (addresses, pin, token) = parse_link(link)?;
    let attempts = net::ordered_addresses(&addresses, &pin);
    let dir = config::device_dir()?;
    let pending_path = dir.join("pending.toml");
    let existing = if dir.join("identity.toml").exists() {
        Some(Identity::load()?)
    } else {
        None
    };
    if existing
        .as_ref()
        .is_some_and(|id| crypto::ca_spki_pin(&id.ca_pem).ok().as_deref() != Some(&pin))
    {
        bail!("DEPLOYMENT_MISMATCH: already joined to another deployment")
    }
    let pending: PendingIdentity = if let Some(id) = &existing {
        PendingIdentity {
            key_pem: id.key_pem.clone(),
            ca_pem: id.ca_pem.clone(),
            pin: pin.clone(),
        }
    } else if pending_path.exists() {
        let value: PendingIdentity = config::read(&pending_path)?;
        if value.pin != pin {
            bail!("DEPLOYMENT_MISMATCH: pending pairing belongs to another deployment")
        }
        value
    } else {
        let mut ca = None;
        let mut error = None;
        for address in &attempts {
            match tokio::time::timeout(Duration::from_secs(5), crypto::discover_ca(address, &pin))
                .await
            {
                Ok(Ok(value)) => {
                    ca = Some(value);
                    break;
                }
                Ok(Err(e)) => error = Some(e),
                Err(_) => error = Some(anyhow::anyhow!("CONNECT_TIMEOUT: {address}")),
            }
        }
        let ca_pem = ca.ok_or_else(|| {
            error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: no reachable address"))
        })?;
        let (key_pem, _) = crypto::new_device_request()?;
        let value = PendingIdentity {
            key_pem,
            ca_pem,
            pin: pin.clone(),
        };
        config::write(&pending_path, &value)?;
        value
    };
    let name = name
        .or_else(|| existing.as_ref().map(|i| i.name.clone()))
        .unwrap_or_else(user_name);
    if !valid_name(&name) {
        bail!("INVALID_NAME: use [a-z][a-z0-9-]{{0,31}} and avoid command names")
    }
    let client = crypto::http_client(&pending.ca_pem, None)?;
    let csr = crypto::renew_device_request(&pending.key_pem)?;
    let body = PairRequest {
        token,
        name,
        csr_base64: STANDARD.encode(csr),
    };
    let mut error = None;
    for address in &attempts {
        match tokio::time::timeout(
            Duration::from_secs(5),
            client
                .post(format!("{address}/pair"))
                .header("x-xrun-version", VERSION)
                .json(&body)
                .send(),
        )
        .await
        {
            Ok(Ok(r)) => {
                if !r.status().is_success() {
                    let text = r.text().await?;
                    if let Ok(Data::Error { code, message }) = serde_json::from_str(&text) {
                        bail!("{code}: {message}")
                    }
                    bail!("PAIR_FAILED: {text}")
                };
                let pair: PairResponse = r.json().await?;
                let id = Identity {
                    device_id: pair.device_id,
                    name: pair.name,
                    addresses: addresses.clone(),
                    ca_pem: pending.ca_pem.clone(),
                    cert_pem: pair.cert_pem,
                    key_pem: pending.key_pem.clone(),
                    registration: pair.registration,
                };
                if existing
                    .as_ref()
                    .is_some_and(|old| old.device_id != id.device_id)
                {
                    bail!("IDENTITY_MISMATCH: server changed existing identity")
                };
                id.save()?;
                net::remember(&id, address);
                if pending_path.exists() {
                    std::fs::remove_file(&pending_path)?;
                    config::sync_parent(&pending_path)?
                }
                return Ok(id);
            }
            Ok(Err(e)) => error = Some(e.into()),
            Err(_) => error = Some(anyhow::anyhow!("CONNECT_TIMEOUT: {address}")),
        }
    }
    Err(error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: pairing unavailable")))
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
        bail!("NO_ADDRESS: provide --addr <host>:<port>")
    }
    Ok(addresses)
}
fn deployment_urls(cfg: &ServerConfig) -> Vec<String> {
    let mut urls = vec![format!("https://127.0.0.1:{}", cfg.port)];
    for url in cfg.urls() {
        if !urls.contains(&url) {
            urls.push(url)
        }
    }
    urls
}
async fn server_foreground(cfg: ServerConfig) -> Result<()> {
    tokio::select! {r=crate::server::run(cfg)=>r,_=daemon::shutdown_signal()=>Ok(())}
}
async fn up(
    port: Option<u16>,
    addresses: Vec<String>,
    no_detect: bool,
    no_service: bool,
    no_daemon: bool,
    allow: bool,
    json: bool,
) -> Result<()> {
    require_linux()?;
    let dir = config::device_dir()?;
    let old_addresses = ServerConfig::load().ok().map(|c| c.addresses);
    let mut cfg = if dir.join("config.toml").exists() {
        let cfg = ServerConfig::load()?;
        if port.is_some_and(|p| p != cfg.port) {
            bail!("PORT_IMMUTABLE: existing deployment uses {}", cfg.port)
        }
        cfg
    } else {
        ServerConfig {
            port: port.unwrap_or(9528),
            addresses: vec![],
            manual: false,
            no_detect,
            data_dir: dir.join("server"),
        }
    };
    if cfg.port == 0 {
        bail!("INVALID_PORT: port must be 1..65535")
    }
    if !addresses.is_empty() {
        for a in &addresses {
            validate_address(a)?
        }
        cfg.addresses = addresses;
        cfg.manual = true;
    } else if !cfg.manual {
        cfg.no_detect |= no_detect;
        cfg.addresses = detect_addresses(cfg.port, cfg.no_detect).await?;
    }
    cfg.save()?;
    let keys = crypto::load_or_create_server(&cfg)?;
    let store = ServerStore::open(&cfg.data_dir.join("server.db"))?;
    let current = if dir.join("identity.toml").exists() {
        Some(Identity::load()?)
    } else {
        None
    };
    if current.as_ref().is_some_and(|i| i.ca_pem != keys.ca_pem) {
        bail!("DEPLOYMENT_MISMATCH: local identity belongs to another CA")
    }
    let new_identity = current.is_none();
    if !no_service {
        service::install("server").await?;
        if old_addresses.as_ref().is_some_and(|a| a != &cfg.addresses) {
            service::restart("server").await?;
        }
    }
    let mut running = if no_service {
        let cfg = cfg.clone();
        Some(tokio::spawn(crate::server::run(cfg)))
    } else {
        None
    };
    let id = if let Some(mut id) = current {
        id.addresses = deployment_urls(&cfg);
        id.save()?;
        id
    } else {
        let pending_key = if dir.join("pending.toml").exists() {
            let p: PendingIdentity = config::read(&dir.join("pending.toml"))?;
            Some(crypto::csr_key(&crypto::renew_device_request(&p.key_pem)?)?)
        } else {
            None
        };
        store.recover_admin(pending_key.as_deref())?;
        let token = store.invite(&Invitation {
            inviter_id: None,
            allow: false,
            admin: true,
        })?;
        let link = format!(
            "xrun://127.0.0.1:{}/{}#{token}",
            cfg.port,
            crypto::ca_spki_pin(&keys.ca_pem)?
        );
        let mut result = None;
        for _ in 0..50 {
            match join(&link, Some("admin".into())).await {
                Ok(mut id) => {
                    id.addresses = deployment_urls(&cfg);
                    id.save()?;
                    result = Some(id);
                    break;
                }
                Err(e) => {
                    if running.as_ref().is_some_and(|r| r.is_finished()) {
                        bail!("SERVER_FAILED: server exited during bootstrap")
                    };
                    if !e.to_string().starts_with("CONNECT") {
                        return Err(e);
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        result.context("SERVER_UNAVAILABLE: bootstrap timed out")?
    };
    daemon::init()?;
    if !no_daemon && !no_service {
        service::install("daemon").await?;
        if new_identity {
            service::restart("daemon").await?;
        }
    }
    let invitation = store.invite(&Invitation {
        inviter_id: Some(id.device_id.clone()),
        allow,
        admin: false,
    })?;
    let link = format!(
        "xrun://{}/{}#{invitation}",
        cfg.addresses.join(","),
        crypto::ca_spki_pin(&keys.ca_pem)?
    );
    print(
        json,
        &serde_json::json!({"device_id":id.device_id,"link":link,"addresses":cfg.addresses,"allow":allow}),
        || {
            println!("server: {}", cfg.addresses.join(", "));
            println!("xrun join '{link}'");
        },
    );
    invitation_notice(allow);
    eprintln!(
        "[xrun] allow inbound TCP {} in your firewall or cloud security group",
        cfg.port
    );
    if let Some(mut server) = running.take() {
        if no_daemon {
            tokio::select! {r=&mut server=>r??,_=daemon::shutdown_signal()=>{}}
        } else {
            tokio::select! {r=&mut server=>r??,r=daemon::run()=>r?}
        }
        server.abort();
    }
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

struct Session {
    ws: Ws,
    db_id: String,
    cwd: String,
}
async fn session(id: &Identity, target: &str) -> Result<Session> {
    let (mut ws, _) = net::websocket(id, &format!("/devices/{target}/session")).await?;
    let data =
        tokio::time::timeout(Duration::from_secs(12), net::receive::<Data>(&mut ws)).await??;
    match data {
        Data::Ready {
            version,
            device_id,
            db_id,
            default_cwd,
        } => {
            if version != VERSION {
                bail!("VERSION_MISMATCH: daemon runs {version}, CLI runs {VERSION}")
            }
            if device_id != target {
                bail!("DEVICE_MISMATCH: session connected to a different device")
            };
            Ok(Session {
                ws,
                db_id,
                cwd: default_cwd,
            })
        }
        Data::Error { code, message } => bail!("{code}: {message}"),
        _ => bail!("INVALID_MESSAGE: expected ready"),
    }
}
async fn response(ws: &mut Ws) -> Result<Data> {
    match net::receive::<Data>(ws).await? {
        Data::Error { code, message } => bail!("{code}: {message}"),
        value => Ok(value),
    }
}
async fn request(id: &Identity, target: &str, req: Request) -> Result<Data> {
    let mut s = session(id, target).await?;
    net::send(&mut s.ws, &Data::Request { request: req }).await?;
    response(&mut s.ws).await
}
fn job_ref(job: &Job) -> String {
    format!("{}/{}", job.target_device_id, job.job_id)
}
fn parse_job(value: &str, target: &str, name: &str) -> Result<String> {
    let id = if let Some((device, id)) = value.split_once('/') {
        if device != target && device != name {
            bail!("INVALID_JOB_REF: job belongs to another device")
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
        bail!("INVALID_JOB_REF: expected a six-character job ID")
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
        bail!("INPUT_TOO_LARGE: maximum {max} bytes")
    }
    Ok(bytes)
}

fn file_input() -> Result<Vec<u8>> {
    let mut spool = tempfile::Builder::new().prefix("xrun-push-").tempfile()?;
    let count = std::io::copy(&mut std::io::stdin().take(MAX_FILE + 1), &mut spool)?;
    if count > MAX_FILE {
        bail!("INPUT_TOO_LARGE: maximum {MAX_FILE} bytes");
    }
    spool.as_file().sync_all()?;
    crate::transfer::read_file(spool.path())
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
    let submissions = SubmissionStore::open(&config::device_dir()?.join("submissions.sqlite"))?;
    let prior = explicit.map(|r| submissions.get(r)).transpose()?.flatten();
    if prior
        .as_ref()
        .is_some_and(|p| cli.device != p.target_device_id && cli.device != p.target_name)
    {
        diagnostic(
            json,
            &anyhow::anyhow!("DEVICE_MISMATCH: request-id belongs to another target device"),
        );
        return Ok(2);
    }
    if prior.as_ref().is_some_and(|s| {
        s.source_device_id != id.device_id
            || crypto::ca_spki_pin(&id.ca_pem).ok().as_deref() != Some(&s.ca_pin)
    }) {
        bail!("IDENTITY_MISMATCH: submission belongs to another identity or deployment")
    }
    let selected = prior
        .as_ref()
        .map(|s| s.target_device_id.as_str())
        .unwrap_or(&cli.device);
    let target: Device = net::http(
        &id,
        reqwest::Method::GET,
        &format!("/devices/{selected}"),
        None,
    )
    .await?;
    let target_metadata = target.clone();
    let target_name = target.name.clone();
    let target = &target.device_id;
    match cli.command {
        Remote::Run(e) => {
            Box::pin(execute_cli(
                &id,
                (target, &target_name),
                e,
                false,
                json,
                &submissions,
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
                &submissions,
                prior,
            ))
            .await
        }
        Remote::Info => {
            print(json, &target_metadata, || {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&target_metadata).unwrap()
                )
            });
            Ok(0)
        }
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
                _ => bail!("INVALID_MESSAGE: expected jobs"),
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
                            &anyhow::anyhow!("WAIT_TIMEOUT: task continues running"),
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
                        if net::explicit(&e) && !e.to_string().starts_with("DEVICE_OFFLINE") {
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
                    "UNCONFIRMED: cancellation response timed out"
                )),
            };
            match result {
                Ok(Data::Job { job }) => {
                    show_job(json, &job);
                    Ok(0)
                }
                Ok(_) => bail!("INVALID_MESSAGE: expected job"),
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
            let bytes = if local == "-" {
                file_input()?
            } else {
                crate::transfer::read_file(Path::new(&local))?
            };
            let size = bytes.len() as u64;
            let hash = sha256(&bytes);
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
                net::send_bytes(&mut s.ws, &bytes).await?;
                response(&mut s.ws).await
            };
            let result = tokio::select! {
                r=operation=>r,
                _=tokio::signal::ctrl_c()=>{return Ok(if sent.load(std::sync::atomic::Ordering::SeqCst){diagnostic(json,&anyhow::anyhow!("UNCONFIRMED: upload interrupted; pull the destination before retrying"));75}else{130});},
                _=termination()=>{return Ok(if sent.load(std::sync::atomic::Ordering::SeqCst){75}else{125});},
            };
            match result {
                Ok(Data::File { path, .. }) => {
                    print(
                        json,
                        &serde_json::json!({"device_id":target,"remote_path":path,"size":size,"sha256":hash}),
                        || println!("{path}"),
                    );
                    Ok(0)
                }
                Ok(_) => bail!("INVALID_MESSAGE: expected file confirmation"),
                Err(e) => {
                    diagnostic(json, &e);
                    Ok(
                        if net::explicit(&e) || e.to_string().starts_with("DEVICE_BUSY") {
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
                bail!("INVALID_MESSAGE: expected file header")
            };
            let bytes = net::receive_bytes(&mut s.ws, size, &sha256, MAX_FILE).await?;
            if local.as_deref() == Some("-") {
                std::io::stdout().write_all(&bytes)?;
                return Ok(0);
            }
            let local = save_download(&bytes, local.map(PathBuf::from), "xrun-pull-", "")?;
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
                bail!("INVALID_MESSAGE: expected screenshot header")
            };
            let bytes = net::receive_bytes(&mut s.ws, size, &sha256, MAX_FILE).await?;
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
fn network_error(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    [
        "CONNECT",
        "SESSION_UNAVAILABLE",
        "SESSION_REJECTED",
        "HTTP_ERROR",
    ]
    .iter()
    .any(|s| text.starts_with(s))
        || error.chain().any(|e| {
            e.downcast_ref::<reqwest::Error>()
                .is_some_and(|e| e.is_connect() || e.is_timeout())
                || e.downcast_ref::<tokio_tungstenite::tungstenite::Error>()
                    .is_some()
        })
}
fn definitive(error: &anyhow::Error) -> bool {
    let code = error.to_string();
    [
        "DEVICE_BUSY",
        "DB_RESET",
        "REQUEST_CONFLICT",
        "SOURCE_NOT_ALLOWED",
        "JOB_NOT_FOUND",
        "STALE",
        "ALREADY_EXISTS",
        "FILE_TOO_LARGE",
        "FILE_NOT_FOUND",
        "FILE_BUSY",
        "INVALID_PATH",
        "INVALID_REQUEST",
        "INVALID_CWD",
        "INVALID_SCRIPT",
        "INVALID_BODY",
        "CHECKSUM_MISMATCH",
        "SHELL_UNSUPPORTED",
        "IS_DIRECTORY",
        "PARENT_NOT_FOUND",
        "PERMISSION_DENIED",
    ]
    .iter()
    .any(|c| code.starts_with(c))
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
            &anyhow::anyhow!("INVALID_SHELL: select sh, bash, zsh, powershell, pwsh or cmd"),
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
                &anyhow::anyhow!("REQUEST_CONFLICT: original execution parameters must match"),
            );
            return Ok(2);
        }
        if prior.db_id != s.db_id {
            diagnostic(
                json,
                &anyhow::anyhow!("DB_RESET: original task database no longer exists"),
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
        },
    };
    if serde_json::to_vec(&header)?.len() > MAX_MESSAGE {
        diagnostic(
            json,
            &anyhow::anyhow!("INVALID_COMMAND: execution header exceeds 1 MiB"),
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
                },
            },
        )
        .await?;
        net::send_bytes(&mut s.ws, &input).await?;
        match response(&mut s.ws).await? {
            Data::Job { job } => Ok(job),
            _ => bail!("INVALID_MESSAGE: expected job acknowledgement"),
        }
    };
    let job = tokio::select! {
        r=submitted=>match r{Ok(job)=>job,Err(error)=>{
            if definitive(&error){submission.status="not_accepted".into();store.save(&submission)?;diagnostic(json,&error);return Ok(125)}
            match recover(id,target,&execution.request_id,Some(&execution.db_id)).await{Ok(job)=>job,Err(e)=>{if e.to_string().starts_with("DB_RESET"){diagnostic(json,&e);return Ok(125)}diagnostic(json,&anyhow::anyhow!("UNCONFIRMED: request {} may have executed; use recent or jobs --request-id",execution.request_id));return Ok(75)}}
        }},
        _=tokio::signal::ctrl_c()=>{if !sent.load(std::sync::atomic::Ordering::SeqCst){submission.status="not_accepted".into();store.save(&submission)?;return Ok(130)}return Box::pin(cancel_unknown(id,target,&execution.request_id,&execution.db_id,json)).await;},
        _=termination()=>{if !sent.load(std::sync::atomic::Ordering::SeqCst){submission.status="not_accepted".into();store.save(&submission)?;return Ok(125)}diagnostic(json,&anyhow::anyhow!("UNCONFIRMED: request {} may have executed",execution.request_id));return Ok(75)}
    };
    submission.job_id = Some(job_ref(&job));
    submission.status = "confirmed".into();
    store.save(&submission)?;
    if background {
        print(json, &job, || println!("{target_name}/{}", job.job_id));
        return Ok(0);
    }
    let mut cursor = LogCursor {
        after: 0,
        db_id: Some(job.db_id.clone()),
        received: false,
    };
    let mut deadline = None;
    loop {
        let result = {
            let logs = stream_logs(id, target, &job.job_id, true, false, &mut cursor);
            tokio::pin!(logs);
            tokio::select! {
                r=&mut logs=>r,
                _=tokio::signal::ctrl_c()=>{return Box::pin(cancel_known(id,target,&job.job_id,json)).await},
                _=termination()=>{diagnostic(json,&anyhow::anyhow!("UNCONFIRMED: {} continues remotely",job_ref(&job)));return Ok(75)}
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
                if error.to_string().starts_with("DB_RESET") {
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
                            "UNCONFIRMED: result unavailable for {}: {error}",
                            job_ref(&job)
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
                        bail!("DB_RESET: original task database no longer exists")
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
                    if let Data::Jobs { jobs } = response(&mut s.ws).await?
                        && let Some(job) = jobs.into_iter().next()
                    {
                        return Ok(job);
                    }
                }
                Err(e) if net::explicit(&e) && !e.to_string().starts_with("DEVICE_OFFLINE") => {
                    return Err(e);
                }
                Err(_) => {}
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await
    .context("UNCONFIRMED: request recovery timed out")?
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
                &anyhow::anyhow!("UNCONFIRMED: cancellation unconfirmed for request {request_id}"),
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
                &anyhow::anyhow!("UNCONFIRMED: cancellation unconfirmed for {target}/{job}"),
            );
            Ok(75)
        }
    }
}
async fn wait(id: &Identity, target: &str, job: &str) -> Result<Job> {
    match request(id, target, Request::Wait { id: job.into() }).await? {
        Data::Job { job } => Ok(job),
        _ => bail!("INVALID_MESSAGE: expected final job state"),
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
        bail!("DB_RESET: original task database no longer exists")
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
    let mut final_job = None;
    loop {
        match response(&mut s.ws).await? {
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
            Data::End => return final_job.context("INVALID_MESSAGE: logs ended without job state"),
            _ => bail!("INVALID_MESSAGE: expected logs"),
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
                return Ok((
                    events,
                    state.context("INVALID_MESSAGE: logs ended without job state")?,
                ));
            }
            _ => bail!("INVALID_MESSAGE: expected logs"),
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
