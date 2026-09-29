use crate::{
    agent,
    config::{Identity, ServerConfig, atomic_private_write, device_dir},
    crypto,
    protocol::*,
    server,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::{Args, Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeMap,
    io::{IsTerminal, Read, Write},
    path::{Path, PathBuf},
};
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};

#[derive(Parser)]
#[command(name = "xrun", version, about = "Run commands on paired devices")]
pub struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Server {
        #[arg(long)]
        config: PathBuf,
    },
    Pair {
        #[arg(long, default_value = "server.toml")]
        config: PathBuf,
        #[arg(long)]
        renew: Option<String>,
    },
    Revoke {
        #[arg(long, default_value = "server.toml")]
        config: PathBuf,
        device: String,
    },
    Join {
        url: String,
        #[arg(long)]
        name: Option<String>,
    },
    Renew,
    Agent {
        #[command(subcommand)]
        command: Option<AgentCommand>,
    },
    Ls,
    Info {
        device: String,
    },
    Exec(ExecArgs),
    Jobs {
        #[arg(long)]
        device: Option<String>,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    Job {
        job_id: String,
    },
    Logs {
        job_id: String,
        #[arg(long)]
        follow: bool,
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
    Kill {
        job_id: String,
    },
}

#[derive(Subcommand)]
enum AgentCommand {
    Init,
}

#[derive(Args)]
struct ExecArgs {
    device: String,
    #[arg(short = 'C')]
    cwd: Option<String>,
    #[arg(long="env",value_parser=parse_env)]
    env: Vec<(String, String)>,
    #[arg(long)]
    stdin: bool,
    #[arg(long, default_value_t = 1800)]
    timeout: u64,
    #[arg(long)]
    detach: bool,
    #[arg(long)]
    request_id: Option<String>,
    #[arg(required = true, last = true)]
    command: Vec<String>,
}

fn parse_env(s: &str) -> std::result::Result<(String, String), String> {
    let (k, v) = s.split_once('=').ok_or("expected KEY=VALUE")?;
    if k.is_empty() || k.contains('\0') || k.contains('=') || v.contains('\0') {
        return Err("invalid environment variable".into());
    }
    Ok((k.into(), v.into()))
}

fn normalize_argv() -> Vec<String> {
    let mut args: Vec<String> = std::env::args().collect();
    let commands = [
        "server", "pair", "revoke", "join", "renew", "agent", "ls", "info", "exec", "jobs", "job",
        "logs", "kill", "help",
    ];
    let mut i = 1;
    while i < args.len() && args[i] == "--json" {
        i += 1;
    }
    if i < args.len() && !args[i].starts_with('-') && !commands.contains(&args[i].as_str()) {
        args.insert(i, "exec".into());
    }
    args
}

pub async fn run() -> Result<i32> {
    let cli = <Cli as Parser>::parse_from(normalize_argv());
    match cli.command {
        Command::Server { config } => {
            server::run(ServerConfig::load(&config)?).await?;
            Ok(0)
        }
        Command::Pair { config, renew } => {
            let v = admin(&config, serde_json::json!({"command":"pair","renew":renew})).await?;
            println!("{v}");
            Ok(0)
        }
        Command::Revoke { config, device } => {
            let v = admin(
                &config,
                serde_json::json!({"command":"revoke","device":device}),
            )
            .await?;
            println!("{v}");
            Ok(0)
        }
        Command::Join { url, name } => {
            join(&url, name).await?;
            Ok(0)
        }
        Command::Renew => {
            let mut id = Identity::load()?;
            crypto::renew_identity(&mut id).await?;
            println!("renewed {}", id.device_id);
            Ok(0)
        }
        Command::Agent {
            command: Some(AgentCommand::Init),
        } => {
            agent::init()?;
            println!("Agent store initialized");
            Ok(0)
        }
        Command::Agent { command: None } => {
            agent::run().await?;
            Ok(0)
        }
        Command::Ls => {
            let id = load_identity().await?;
            let d: Vec<Device> = get(&id, "/devices").await?;
            if cli.json {
                println!("{}", serde_json::to_string(&d)?);
            } else {
                for x in d {
                    println!(
                        "{}\t{}\t{}",
                        x.name,
                        x.device_id,
                        if x.online { "online" } else { "offline" }
                    );
                }
            }
            Ok(0)
        }
        Command::Info { device } => {
            let id = load_identity().await?;
            let d: Device = get(&id, &format!("/devices/{device}")).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&d)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&d)?);
            }
            Ok(0)
        }
        Command::Jobs {
            device,
            request_id,
            limit,
            offset,
        } => {
            let id = load_identity().await?;
            let mut q = url::form_urlencoded::Serializer::new(String::new());
            if let Some(d) = device {
                q.append_pair("device", &d);
            }
            if let Some(r) = request_id {
                q.append_pair("request_id", &r);
            }
            q.append_pair("limit", &limit.to_string())
                .append_pair("offset", &offset.to_string());
            let j: Vec<Job> = get(&id, &format!("/jobs?{}", q.finish())).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&j)?);
            } else {
                for x in j {
                    println!(
                        "{}\t{}\t{}",
                        x.job_id,
                        x.target_device_id,
                        serde_json::to_string(&x.state)?.trim_matches('"')
                    );
                }
            }
            Ok(0)
        }
        Command::Job { job_id } => {
            let id = load_identity().await?;
            let j: Job = get(&id, &format!("/jobs/{job_id}")).await?;
            if cli.json {
                println!("{}", serde_json::to_string(&j)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&j)?);
            }
            Ok(0)
        }
        Command::Kill { job_id } => {
            let id = load_identity().await?;
            let j: Job = post(
                &id,
                &format!("/jobs/{job_id}/cancel"),
                &serde_json::json!({}),
            )
            .await?;
            if cli.json {
                println!("{}", serde_json::to_string(&j)?);
            } else {
                eprintln!("[xrun] {}: {:?}", j.job_id, j.state);
            }
            Ok(0)
        }
        Command::Logs {
            job_id,
            follow,
            after,
        } => {
            let id = load_identity().await?;
            let j = logs(&id, &job_id, after, follow, cli.json).await?;
            Ok(if follow { exit_status(&j) } else { 0 })
        }
        Command::Exec(args) => exec(args, cli.json).await,
    }
}

#[cfg(unix)]
async fn admin(config: &Path, request: serde_json::Value) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let cfg = ServerConfig::load(config)?;
    let mut sock = tokio::net::UnixStream::connect(cfg.data_dir.join("admin.sock")).await?;
    sock.write_all(serde_json::to_string(&request)?.as_bytes())
        .await?;
    sock.shutdown().await?;
    let mut buf = Vec::new();
    sock.read_to_end(&mut buf).await?;
    let response: serde_json::Value = serde_json::from_slice(&buf)?;
    if response["ok"] != true {
        bail!("{}", response["value"]);
    }
    Ok(response["value"].as_str().unwrap_or_default().into())
}
#[cfg(not(unix))]
async fn admin(_: &Path, _: serde_json::Value) -> Result<String> {
    bail!("Server management is Linux-only in v1")
}

async fn join(link: &str, name: Option<String>) -> Result<()> {
    let mut url = url::Url::parse(link)?;
    let fragment = url.fragment().context("pairing link lacks trust data")?;
    let parts: BTreeMap<String, String> = url::form_urlencoded::parse(fragment.as_bytes())
        .into_owned()
        .collect();
    let token = parts.get("token").context("pairing token missing")?;
    let pin = parts.get("ca").context("CA pin missing")?;
    let ca =
        crypto::verify_ca_from_link(parts.get("cert").context("CA certificate missing")?, pin)?;
    url.set_fragment(None);
    let dir = device_dir()?;
    std::fs::create_dir_all(&dir)?;
    crate::config::restrict_dir(&dir)?;
    let existing = if dir.join("identity.toml").exists() {
        Some(Identity::load()?)
    } else {
        None
    };
    if let Some(id) = &existing
        && (crypto::ca_spki_pin(&id.ca_pem)? != *pin
            || id.server_url != url.origin().ascii_serialization())
    {
        bail!("recovery link does not match existing server identity");
    }
    let pending = dir.join("pending.key");
    let (key, csr) = if let Some(id) = &existing {
        (
            id.key_pem.clone(),
            crypto::renew_device_request(&id.key_pem)?,
        )
    } else if pending.exists() {
        let key = std::fs::read_to_string(&pending)?;
        let csr = crypto::renew_device_request(&key)?;
        (key, csr)
    } else {
        let (key, csr) = crypto::new_device_request()?;
        atomic_private_write(&pending, key.as_bytes())?;
        (key, csr)
    };
    let client = crypto::http_client(&ca, None)?;
    let response = client
        .post(url)
        .header("x-xrun-version", VERSION)
        .json(&PairRequest {
            token: token.clone(),
            name,
            csr_base64: STANDARD.encode(csr),
        })
        .send()
        .await?;
    let pair: PairResponse = decode(response).await?;
    if crypto::ca_spki_pin(&pair.ca_pem)? != *pin {
        bail!("server returned a different CA");
    }
    if let Some(old) = &existing
        && (old.device_id != pair.device_id || old.name != pair.name)
    {
        bail!("recovery returned a different device identity");
    }
    let id = Identity {
        device_id: pair.device_id.clone(),
        name: pair.name.clone(),
        server_url: pair.server_url,
        ca_pem: ca,
        cert_pem: pair.cert_pem,
        key_pem: key,
    };
    id.save()?;
    if pending.exists() {
        std::fs::remove_file(pending)?;
    }
    println!("paired {} as {}", pair.name, pair.device_id);
    Ok(())
}

async fn load_identity() -> Result<Identity> {
    let mut id = Identity::load()?;
    if crypto::certificate_expiring(&id.cert_pem, 30)? {
        crypto::renew_identity(&mut id).await?;
    }
    Ok(id)
}

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    let body = response.bytes().await?;
    if !status.is_success() {
        let detail = serde_json::from_slice::<ApiError>(&body)
            .map(|e| format!("{}: {}", e.error.code, e.error.message))
            .unwrap_or_else(|_| String::from_utf8_lossy(&body).to_string());
        bail!("HTTP {status}: {detail}");
    }
    Ok(serde_json::from_slice(&body)?)
}

async fn get<T: DeserializeOwned>(id: &Identity, path: &str) -> Result<T> {
    decode(
        crypto::http_client(&id.ca_pem, Some(id))?
            .get(format!("{}{}", id.server_url, path))
            .header("x-xrun-version", VERSION)
            .send()
            .await?,
    )
    .await
}
async fn post<T: DeserializeOwned, U: Serialize>(
    id: &Identity,
    path: &str,
    payload: &U,
) -> Result<T> {
    decode(
        crypto::http_client(&id.ca_pem, Some(id))?
            .post(format!("{}{}", id.server_url, path))
            .header("x-xrun-version", VERSION)
            .json(payload)
            .send()
            .await?,
    )
    .await
}

async fn exec(args: ExecArgs, json: bool) -> Result<i32> {
    let id = load_identity().await?;
    let request_id = args
        .request_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if !json {
        eprintln!("[xrun] request_id={request_id}");
    } else {
        println!(
            "{}",
            serde_json::json!({"type":"request","request_id":request_id})
        );
    }
    let mut stdin = Vec::new();
    if args.stdin {
        if std::io::stdin().is_terminal() {
            bail!("--stdin requires a pipe or redirection");
        }
        std::io::stdin()
            .take((MAX_STDIN + 1) as u64)
            .read_to_end(&mut stdin)?;
        if stdin.len() > MAX_STDIN {
            bail!("STDIN_TOO_LARGE: stdin exceeds 1 MiB");
        }
    }
    let env = args.env.into_iter().collect();
    let req = ExecRequest {
        request_id,
        target_device_id: args.device,
        program: args.command[0].clone(),
        args: args.command[1..].to_vec(),
        cwd: args.cwd,
        env,
        stdin_base64: if stdin.is_empty() {
            None
        } else {
            Some(STANDARD.encode(stdin))
        },
        timeout_seconds: args.timeout,
    };
    let j: Job = post(&id, "/jobs", &req).await?;
    if json {
        println!("{}", serde_json::json!({"type":"accepted","job":j}));
    } else {
        eprintln!("[xrun] job_id={}", j.job_id);
    }
    if args.detach {
        return Ok(0);
    }
    let final_job = tokio::select! {
        result=logs(&id,&j.job_id,0,true,json)=>result?,
        _=tokio::signal::ctrl_c()=>{
            let result:Result<Job>=post(&id,&format!("/jobs/{}/cancel",j.job_id),&serde_json::json!({})).await;
            if let Err(e)=result{eprintln!("[xrun] cancel unconfirmed for {}: {e}",j.job_id);}
            return Ok(130);
        }
    };
    Ok(exit_status(&final_job))
}

fn exit_status(j: &Job) -> i32 {
    if j.state == JobState::Exited
        && j.output_complete
        && let Some(code) = j.exit_code
        && (0..=255).contains(&code)
    {
        return code as i32;
    }
    125
}

async fn logs(id: &Identity, job_id: &str, after: u64, follow: bool, json: bool) -> Result<Job> {
    let url = format!(
        "{}/jobs/{job_id}/logs?after={after}&follow={follow}",
        id.server_url.replace("https://", "wss://")
    );
    let mut req = url.into_client_request()?;
    req.headers_mut()
        .insert("x-xrun-version", HeaderValue::from_static(VERSION));
    let tls = crypto::client_tls_config(id)?;
    let (mut socket, _) =
        connect_async_tls_with_config(req, None, false, Some(Connector::Rustls(tls))).await?;
    let mut last = after;
    let mut gap = false;
    while let Some(frame) = socket.next().await {
        match frame? {
            Message::Text(text) => {
                let v: serde_json::Value = serde_json::from_str(&text)?;
                match v["type"].as_str() {
                    Some("output") => {
                        let e: LogEvent = serde_json::from_value(v["event"].clone())?;
                        if e.seq <= last {
                            continue;
                        }
                        if e.seq != last + 1 {
                            gap = true;
                        }
                        last = e.seq;
                        if json {
                            println!("{}", serde_json::json!({"type":"output","event":e}));
                        } else {
                            let data = STANDARD.decode(&e.data_base64)?;
                            if e.stream == "stderr" {
                                std::io::stderr().write_all(&data)?;
                            } else {
                                std::io::stdout().write_all(&data)?;
                            }
                        }
                    }
                    Some("result") => {
                        let j: Job = serde_json::from_value(v["job"].clone())?;
                        if json {
                            println!("{}", serde_json::json!({"type":"result","job":j}));
                        } else {
                            eprintln!("[xrun] {}: {:?}", j.job_id, j.state);
                        }
                        if follow && (gap || last < j.last_seq || !j.output_complete) {
                            bail!(
                                "LOG_UNAVAILABLE: output incomplete for {} (received through seq {last}, expected {}, gap={gap}, agent_capture_complete={})",
                                j.job_id,
                                j.last_seq,
                                j.output_complete
                            );
                        }
                        return Ok(j);
                    }
                    Some("snapshot_end") => break,
                    Some("log_source") if json => {
                        println!("{v}");
                    }
                    _ => {}
                }
            }
            Message::Ping(data) => {
                socket.send(Message::Pong(data)).await?;
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
    if follow {
        bail!("LOG_UNAVAILABLE: log stream ended before terminal result for {job_id}");
    }
    if gap {
        bail!("LOG_UNAVAILABLE: log sequence gap for {job_id}");
    }
    let job: Job = get(id, &format!("/jobs/{job_id}")).await?;
    if last < job.last_seq {
        bail!("LOG_UNAVAILABLE: output missing for {job_id} after seq {last}");
    }
    if job.state.terminal() && !job.output_complete {
        bail!("LOG_UNAVAILABLE: agent output capture is incomplete for {job_id}");
    }
    Ok(job)
}
