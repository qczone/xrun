#![allow(dead_code)]
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    path::Path,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::process::{Child, Command};
use xrun::testing::config::{Identity, ServerConfig};

pub fn binary() -> std::path::PathBuf {
    std::env::var_os("XRUN_TEST_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_xrun").into())
        .canonicalize()
        .expect("test binary must exist")
}
pub fn command(home: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(binary());
    cmd.env("HOME", home)
        .env("USERPROFILE", home)
        .args(args)
        .kill_on_drop(true);
    cmd
}
fn process_log_path(home: &Path, name: &str) -> std::path::PathBuf {
    let base = std::env::var_os("XRUN_TEST_LOG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join("test-logs"));
    let hash = xrun::protocol::sha256(home.to_string_lossy().as_bytes());
    base.join(format!("{name}-{}.log", &hash[..12]))
}
pub fn logged(home: &Path, args: &[&str], name: &str) -> Result<Command> {
    let path = process_log_path(home, name);
    std::fs::create_dir_all(path.parent().context("process log directory")?)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut cmd = command(home, args);
    cmd.env("RUST_LOG", "xrun=debug")
        .env("NO_COLOR", "1")
        .stdout(Stdio::from(file.try_clone()?))
        .stderr(Stdio::from(file));
    Ok(cmd)
}
pub fn library_logs() -> Result<()> {
    if let Some(base) = std::env::var_os("XRUN_TEST_LOG_DIR") {
        std::fs::create_dir_all(&base)?;
        let name = std::env::current_exe()?
            .file_name()
            .context("test name")?
            .to_string_lossy()
            .into_owned();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(std::path::Path::new(&base).join(format!("{name}.log")))?;
        let _ = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_env_filter("xrun=debug")
            .with_writer(std::sync::Mutex::new(file))
            .try_init();
    }
    Ok(())
}
pub async fn cli(home: &Path, args: &[&str]) -> Output {
    tokio::time::timeout(Duration::from_secs(45), command(home, args).output())
        .await
        .expect("CLI exceeded deadline; see process logs")
        .unwrap()
}
#[track_caller]
pub fn ok(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
pub fn json(output: Output) -> Value {
    serde_json::from_str(&ok(output)).unwrap()
}
pub async fn online(home: &Path, name: &str) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let out = cli(home, &[name, "info", "--json"]).await;
            if out.status.success()
                && serde_json::from_slice::<Value>(&out.stdout).is_ok_and(|v| v["online"] == true)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("device did not reconnect; see process logs")
}
pub async fn stop_daemon(home: &Path, child: &mut Child) -> Result<()> {
    xrun::testing::control::request_shutdown(&home.join(".xrun")).await?;
    let status = tokio::time::timeout(Duration::from_secs(15), child.wait()).await??;
    anyhow::ensure!(status.success(), "daemon shutdown failed: {status}");
    Ok(())
}

pub struct TestRelay {
    pub config: ServerConfig,
    home: std::path::PathBuf,
    process: Option<Child>,
    task: Option<tokio::task::JoinHandle<Result<()>>>,
}
// Selecting a free port closes its temporary socket before the relay can bind.
// Keep parallel labs from selecting the same port during that handoff.
static RELAY_START: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn reserve_relay_port() -> Result<std::net::TcpListener> {
    let address = std::net::Ipv4Addr::UNSPECIFIED;
    #[cfg(not(target_os = "linux"))]
    return Ok(std::net::TcpListener::bind((address, 0))?);

    #[cfg(target_os = "linux")]
    {
        // Match the relay's wildcard bind and stay outside the range used by
        // outbound connections, which can claim a port during the handoff.
        let bounds = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")?
            .split_whitespace()
            .map(str::parse::<u16>)
            .collect::<std::result::Result<Vec<_>, _>>()?;
        anyhow::ensure!(
            bounds.len() == 2 && bounds[0] <= bounds[1],
            "invalid local port range"
        );
        let lower_count = u32::from(bounds[0].saturating_sub(1024));
        let upper_start = (u32::from(bounds[1]) + 1).max(1024);
        let available = lower_count + 65536 - upper_start;
        anyhow::ensure!(available > 0, "no ports outside the local port range");
        let seed = uuid::Uuid::new_v4();
        let first = u32::from_le_bytes(seed.as_bytes()[..4].try_into()?) % available;
        // Pick within the usable ranges instead of clustering all excluded
        // random starts at the first port after the ephemeral range.
        for offset in 0..available {
            let index = (first + offset) % available;
            let port = u16::try_from(if index < lower_count {
                1024 + index
            } else {
                upper_start + index - lower_count
            })?;
            match std::net::TcpListener::bind((address, port)) {
                Ok(listener) => return Ok(listener),
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                Err(error) => return Err(error.into()),
            }
        }
        anyhow::bail!("no free relay port outside the local port range")
    }
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        if let Some(child) = &mut self.process {
            let _ = child.start_kill();
        }
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}
impl TestRelay {
    pub async fn new(home: &Path) -> Result<(Self, String)> {
        let _starting = RELAY_START.lock().await;
        for _ in 0..8 {
            let reservation = reserve_relay_port()?;
            let port = reservation.local_addr()?.port();
            let config = ServerConfig {
                port,
                addresses: vec![format!("127.0.0.1:{port}")],
                manual: true,
                no_detect: true,
                data_dir: home.join(".xrun/server"),
            };
            xrun::testing::config::write(&home.join(".xrun/config.toml"), &config)?;
            let link = xrun::testing::relay::deployment_link(&config)?;
            let mut relay = Self {
                config,
                home: home.into(),
                process: None,
                task: None,
            };
            drop(reservation);
            match relay.start().await {
                Ok(()) => return Ok((relay, link)),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::AddrInUse) => {}
                Err(error) => return Err(error),
            }
        }
        anyhow::bail!("relay port was repeatedly claimed during startup")
    }
    pub async fn start(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.process.is_none() && self.task.is_none(),
            "relay already started"
        );
        library_logs()?;
        let ca_pem = std::fs::read_to_string(self.config.data_dir.join("ca.pem"))?;
        let probe = xrun::testing::crypto::http_client(&ca_pem, None)?;
        let url = format!("https://127.0.0.1:{}/", self.config.port);
        let log_path = process_log_path(&self.home, "relay");
        let log_offset = std::fs::metadata(&log_path).map_or(0, |m| m.len() as usize);
        if cfg!(target_os = "linux") {
            self.process = Some(logged(&self.home, &["relay", "run"], "relay")?.spawn()?);
        } else {
            // Service deployment is Linux-only; other platforms exercise the
            // same relay library with the selected device binary on both ends.
            self.task = Some(tokio::spawn(xrun::testing::relay::run(self.config.clone())));
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(process) = &mut self.process
                    && let Some(status) = process.try_wait()?
                {
                    let log = std::fs::read(&log_path)?;
                    let startup = String::from_utf8_lossy(log.get(log_offset..).unwrap_or(&[]));
                    if startup
                        .lines()
                        .any(|line| line == "[xrun] Address already in use (os error 98)")
                    {
                        return Err(std::io::Error::from(std::io::ErrorKind::AddrInUse).into());
                    }
                    anyhow::bail!("relay exited with {status}; see process logs");
                }
                if self.task.as_ref().is_some_and(|task| task.is_finished()) {
                    match self.task.take().unwrap().await? {
                        Err(error)
                            if error
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|e| e.kind() == std::io::ErrorKind::AddrInUse) =>
                        {
                            // Restarted listeners can race sockets still releasing the port.
                            self.task =
                                Some(tokio::spawn(xrun::testing::relay::run(self.config.clone())));
                        }
                        Err(error) => {
                            return Err(error.context("test relay exited before readiness"));
                        }
                        Ok(()) => anyhow::bail!("test relay exited before readiness"),
                    }
                }
                // A TCP connect can succeed before our listener is ready. Require
                // an HTTPS response from the relay with this fixture's pinned CA.
                if probe
                    .get(&url)
                    .timeout(Duration::from_millis(500))
                    .send()
                    .await
                    .is_ok()
                {
                    if self.task.as_ref().is_some_and(|task| task.is_finished()) {
                        continue;
                    }
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok::<_, anyhow::Error>(())
        })
        .await??;
        Ok(())
    }
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(mut child) = self.process.take() {
            child.start_kill()?;
            child.wait().await?;
        }
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        Ok(())
    }
}

pub struct Lab {
    pub root: tempfile::TempDir,
    pub source: std::path::PathBuf,
    pub target: std::path::PathBuf,
    pub source_identity: Identity,
    pub target_identity: Identity,
    pub daemon: Child,
    pub source_daemon: Child,
    pub relay: TestRelay,
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = self.daemon.start_kill();
        let _ = self.source_daemon.start_kill();
    }
}
impl Lab {
    pub async fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&target)?;
        let (relay, link) = TestRelay::new(&root.path().join("relay")).await?;
        ok(cli(
            &source,
            &["up", "--relay", &link, "--name", "source1", "--no-daemon"],
        )
        .await);
        let source_daemon = logged(&source, &["daemon"], "manager")?.spawn()?;
        online(&source, "source1").await?;
        let invite = json(cli(&source, &["invite", "--allow", "--json"]).await);
        ok(cli(
            &target,
            &[
                "join",
                invite["link"].as_str().context("invitation")?,
                "--name",
                "target1",
                "--no-daemon",
            ],
        )
        .await);
        let source_identity = xrun::testing::config::read(&source.join(".xrun/identity.toml"))?;
        let target_identity = xrun::testing::config::read(&target.join(".xrun/identity.toml"))?;
        let daemon = logged(&target, &["daemon"], "target")?.spawn()?;
        let lab = Self {
            root,
            source,
            target,
            source_identity,
            target_identity,
            daemon,
            source_daemon,
            relay,
        };
        online(&lab.source, "target1").await?;
        Ok(lab)
    }
}

/// Opens a relay socket and answers its challenge; None pairs anonymously.
pub async fn relay_socket(
    id: Option<&Identity>,
    roster: &xrun::testing::membership::SignedRoster,
    path: &str,
) -> Result<xrun::testing::net::Ws> {
    let mut ws = xrun::testing::net::websocket_at(
        &roster.roster.relay_addresses[0],
        path,
        xrun::testing::crypto::anonymous_tls_config(&roster.roster.relay_ca_pem)?,
    )
    .await?;
    xrun::testing::network::authenticate(&mut ws, id, &roster.roster.network_id, path, None)
        .await?;
    Ok(ws)
}

pub async fn peer_session(
    home: &Path,
    id: &Identity,
    target: &str,
) -> Result<xrun::testing::net::Ws> {
    use xrun::testing::{membership::RosterCache, net, protocol::RelayMessage, secure};
    let cache = RosterCache::open(&home.join(".xrun/roster.db"))?;
    let network = &id.network.as_ref().context("network identity")?.network_id;
    let roster = cache.load(network)?;
    let mut outer = relay_socket(
        Some(id),
        &roster,
        &format!("/networks/{network}/connect/{target}"),
    )
    .await?;
    assert!(matches!(
        net::receive(&mut outer).await?,
        RelayMessage::Connected { .. }
    ));
    let (mut ws, cert) = secure::client(outer, id, target).await?;
    secure::exchange_client(
        &mut ws,
        &cache,
        network,
        &cert,
        target,
        &secure::Purpose::Execute,
    )
    .await?;
    Ok(ws)
}
