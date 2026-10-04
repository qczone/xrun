#![allow(dead_code)]
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    path::Path,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::process::{Child, Command};
use xrun::config::{Identity, ServerConfig};

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
pub fn logged(home: &Path, args: &[&str], name: &str) -> Result<Command> {
    let base = std::env::var_os("XRUN_TEST_LOG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join("test-logs"));
    std::fs::create_dir_all(&base)?;
    let hash = xrun::protocol::sha256(home.to_string_lossy().as_bytes());
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(base.join(format!("{name}-{}.log", &hash[..12])))?;
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
    xrun::control::request_shutdown(&home.join(".xrun"))?;
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
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let config = ServerConfig {
            port,
            addresses: vec![format!("127.0.0.1:{port}")],
            manual: true,
            no_detect: true,
            data_dir: home.join(".xrun/server"),
        };
        xrun::config::write(&home.join(".xrun/config.toml"), &config)?;
        let link = xrun::relay::deployment_link(&config)?;
        let mut relay = Self {
            config,
            home: home.into(),
            process: None,
            task: None,
        };
        relay.start().await?;
        Ok((relay, link))
    }
    pub async fn start(&mut self) -> Result<()> {
        anyhow::ensure!(
            self.process.is_none() && self.task.is_none(),
            "relay already started"
        );
        library_logs()?;
        if cfg!(target_os = "linux") {
            self.process = Some(logged(&self.home, &["relay", "run"], "relay")?.spawn()?);
        } else {
            // Service deployment is Linux-only; other platforms exercise the
            // same relay library with the selected device binary on both ends.
            self.task = Some(tokio::spawn(xrun::relay::run(self.config.clone())));
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            while tokio::net::TcpStream::connect(("127.0.0.1", self.config.port))
                .await
                .is_err()
            {
                if let Some(process) = &mut self.process {
                    anyhow::ensure!(
                        process.try_wait()?.is_none(),
                        "relay exited; see process logs"
                    );
                }
                if let Some(task) = &self.task {
                    anyhow::ensure!(!task.is_finished(), "relay task exited; see process logs");
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
        let source_identity = xrun::config::read(&source.join(".xrun/identity.toml"))?;
        let target_identity = xrun::config::read(&target.join(".xrun/identity.toml"))?;
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

pub async fn relay_socket(
    id: &Identity,
    roster: &xrun::membership::SignedRoster,
    path: &str,
) -> Result<xrun::net::Ws> {
    let _ = id;
    xrun::net::websocket_at(
        &roster.roster.relay_addresses[0],
        path,
        xrun::crypto::anonymous_tls_config(&roster.roster.relay_ca_pem)?,
    )
    .await
}

pub async fn peer_session(home: &Path, id: &Identity, target: &str) -> Result<xrun::net::Ws> {
    use xrun::{membership::RosterCache, net, relay::RelayMessage, secure};
    let cache = RosterCache::open(&home.join(".xrun/roster.db"))?;
    let network = &id.network.as_ref().context("network identity")?.network_id;
    let roster = cache.load(network)?;
    let mut outer = relay_socket(
        id,
        &roster,
        &format!("/networks/{network}/connect/{target}"),
    )
    .await?;
    assert!(matches!(
        net::receive(&mut outer).await?,
        RelayMessage::Connected
    ));
    let (mut ws, cert) = secure::client(outer, id, target).await?;
    secure::exchange_client(&mut ws, &cache, network, &cert, target).await?;
    net::send(&mut ws, &secure::Purpose::Execute).await?;
    Ok(ws)
}
