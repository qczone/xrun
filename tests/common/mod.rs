#![allow(dead_code)]
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    path::Path,
    process::{Output, Stdio},
    time::Duration,
};
use tokio::process::{Child, Command};
use xrun::{
    config::{Identity, ServerConfig},
    crypto,
    store::{Invitation, ServerStore},
};

pub struct Lab {
    pub root: tempfile::TempDir,
    pub source: std::path::PathBuf,
    pub target: std::path::PathBuf,
    pub source_identity: Identity,
    pub target_identity: Identity,
    pub daemon: Child,
    server: tokio::task::JoinHandle<Result<()>>,
}
impl Drop for Lab {
    fn drop(&mut self) {
        let _ = self.daemon.start_kill();
        self.server.abort();
    }
}
pub fn command(home: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xrun"));
    cmd.env("HOME", home)
        .env("USERPROFILE", home)
        .args(args)
        .kill_on_drop(true);
    cmd
}
pub async fn cli(home: &Path, args: &[&str]) -> Output {
    tokio::time::timeout(Duration::from_secs(45), command(home, args).output())
        .await
        .unwrap()
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
impl Lab {
    pub async fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let target = root.path().join("target");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&target)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let cfg = ServerConfig {
            port,
            addresses: vec![format!("127.0.0.1:{port}")],
            manual: true,
            no_detect: true,
            data_dir: root.path().join("server"),
        };
        let keys = crypto::load_or_create_server(&cfg)?;
        let store = ServerStore::open(&cfg.data_dir.join("server.db"))?;
        let token = store.invite(&Invitation {
            inviter_id: None,
            allow: false,
            admin: true,
        })?;
        let link = format!(
            "xrun://127.0.0.1:{port}/{}#{token}",
            crypto::ca_spki_pin(&keys.ca_pem)?
        );
        let server = tokio::spawn(xrun::server::run(cfg));
        tokio::time::timeout(Duration::from_secs(5), async {
            while tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err()
            {
                assert!(!server.is_finished());
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        ok(cli(
            &source,
            &["join", &link, "--name", "source1", "--no-daemon"],
        )
        .await);
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
        let daemon = command(&target, &["daemon"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let lab = Self {
            root,
            source,
            target,
            source_identity,
            target_identity,
            daemon,
            server,
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if json(cli(&lab.source, &["target1", "info", "--json"]).await)["online"] == true {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        Ok(lab)
    }
}
