use super::common;
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::process::{Child, Command};
use xrun::testing::{config::ServerConfig, protocol::*};
mod access;
mod environment;
mod execution;
mod files;
mod membership;
mod recovery;
#[cfg(target_os = "linux")]
mod service;
const BUILD_ENV: &[&str] = &[
    "CARGO_TARGET_DIR",
    "CARGO_BUILD_TARGET",
    "RUSTUP_TOOLCHAIN",
    "RUST_RECURSION_COUNT",
    "RUSTC",
    "RUSTDOC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
];
struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}
fn command(home: &Path, args: &[&str]) -> Command {
    common::command(home, args)
}
async fn cli(home: &Path, args: &[&str]) -> std::process::Output {
    tokio::time::timeout(Duration::from_secs(45), command(home, args).output())
        .await
        .expect("CLI exceeded deadline")
        .unwrap()
}
async fn input(home: &Path, args: &[&str], bytes: &[u8]) -> std::process::Output {
    use tokio::io::AsyncWriteExt;
    let mut child = command(home, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(bytes).await.unwrap();
    tokio::time::timeout(Duration::from_secs(45), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
#[track_caller]
fn ok(out: std::process::Output) -> String {
    assert!(
        out.status.success(),
        "status={} stdout={} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}
#[track_caller]
fn json(out: std::process::Output) -> Value {
    serde_json::from_str(&ok(out)).unwrap()
}
async fn online(home: &Path, name: &str) {
    for _ in 0..100 {
        let out = cli(home, &[name, "info", "--json"]).await;
        if out.status.success()
            && serde_json::from_slice::<Value>(&out.stdout).is_ok_and(|v| v["online"] == true)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("daemon did not become online")
}
fn daemon(home: &Path) -> Daemon {
    Daemon(
        common::logged(home, &["daemon"], "daemon")
            .unwrap()
            .spawn()
            .unwrap(),
    )
}

struct Suite {
    root: PathBuf,
    source: PathBuf,
    target: PathBuf,
    observer: PathBuf,
    target_id: String,
    admin: String,
    invite: String,
    cfg: ServerConfig,
    runner: PathBuf,
    target_daemon: Daemon,
    _source_daemon: Daemon,
    _observer_daemon: Daemon,
    server: tokio::task::JoinHandle<Result<()>>,
    _temp: tempfile::TempDir,
}
impl Drop for Suite {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Suite {
    async fn new() -> Result<Self> {
        common::library_logs()?;
        let temp = tempfile::tempdir()?;
        let root = temp.path().to_path_buf();
        let source = root.join("source");
        #[cfg(not(windows))]
        let target = root.join("target");
        #[cfg(windows)]
        let target = root.join("target &^%XRUN_PATH_TEST%! space");
        std::fs::create_dir_all(&source)?;
        std::fs::create_dir_all(&target)?;
        let fresh = json(cli(&source, &["status", "--json"]).await);
        assert_eq!(fresh["local"]["joined"], false);
        assert!(fresh["devices"].is_null());
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let cfg = ServerConfig {
            port,
            addresses: vec![format!("127.0.0.1:{port}")],
            manual: true,
            no_detect: true,
            data_dir: root.join("server"),
        };
        let link = xrun::testing::relay::deployment_link(&cfg)?;
        let server = tokio::spawn(xrun::testing::relay::run(cfg.clone()));
        tokio::time::timeout(Duration::from_secs(5), async {
            while tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err()
            {
                assert!(!server.is_finished(), "test Server exited during startup");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        ok(cli(
            &source,
            &["up", "--relay", &link, "--name", "admin", "--no-daemon"],
        )
        .await);
        let admin = json(cli(&source, &["status", "--json"]).await)["local"]["device_id"]
            .as_str()
            .unwrap()
            .to_string();
        let _source_daemon = daemon(&source);
        for _ in 0..100 {
            if json(cli(&source, &["status", "--json"]).await)["devices"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["device_id"] == admin && d["online"] == true)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let invite = json(cli(&source, &["invite", "--allow", "--json"]).await)["link"]
            .as_str()
            .unwrap()
            .to_string();
        let joined = json(
            cli(
                &target,
                &[
                    "join",
                    &invite,
                    "--name",
                    "runner1",
                    "--no-daemon",
                    "--json",
                ],
            )
            .await,
        );
        let target_id = joined["device_id"]
            .as_str()
            .context("device ID")?
            .to_string();
        let offline = json(cli(&source, &["runner1", "info", "--json"]).await);
        assert_eq!(offline["online"], false);
        assert_eq!(offline["device_id"], target_id);
        // Exercise the shared task process layer with a polluted daemon environment,
        // including Windows' case-insensitive variable names.

        let mut target_command = common::logged(&target, &["daemon"], "target")?;
        for &name in BUILD_ENV {
            target_command.env(
                if cfg!(windows) {
                    name.to_ascii_lowercase()
                } else {
                    name.into()
                },
                "must-not-leak",
            );
        }
        target_command
            .env("XRUN_ENV_PRESERVED", "keep")
            .env("CARGO_HOME", "cargo-home-kept")
            .env("RUSTUP_HOME", "rustup-home-kept");
        let target_daemon = Daemon(target_command.spawn()?);
        online(&source, "runner1").await;
        online(&target, "admin").await;
        // A daemon rejection should print its code once, and keep the JSON message
        // separate from the code even after the CLI formats the remote error.
        let invalid_cwd = cli(&source, &["runner1", "-C", "github/xrun", "--", "unused"]).await;
        assert_eq!(invalid_cwd.status.code(), Some(125));
        assert_eq!(
            String::from_utf8_lossy(&invalid_cwd.stderr).trim(),
            "[xrun] INVALID_REQUEST: cwd must be absolute"
        );
        let invalid_cwd = cli(
            &source,
            &["runner1", "--json", "-C", "github/xrun", "--", "unused"],
        )
        .await;
        assert_eq!(invalid_cwd.status.code(), Some(125));
        let error: Value = serde_json::from_slice(&invalid_cwd.stderr)?;
        assert_eq!(error["code"], "INVALID_REQUEST");
        assert_eq!(error["message"], "cwd must be absolute");
        // An ordinary invitation only registers a device, in both directions.
        // All-member mode includes devices registered after it was enabled.
        ok(cli(&target, &["allow-from", "--all"]).await);
        let observer = root.join("observer");
        std::fs::create_dir_all(&observer)?;
        let registration = json(cli(&source, &["invite", "--json"]).await);
        assert_eq!(registration["allow"], false);
        ok(cli(
            &observer,
            &[
                "join",
                registration["link"].as_str().unwrap(),
                "--name",
                "observer",
                "--no-daemon",
            ],
        )
        .await);
        let _observer_daemon = daemon(&observer);
        online(&source, "observer").await;
        for (caller, device) in [(&source, "observer"), (&observer, "admin")] {
            let denied = cli(caller, &[device, "--", "unused"]).await;
            assert_eq!(denied.status.code(), Some(125));
            assert!(String::from_utf8_lossy(&denied.stderr).contains("SOURCE_NOT_ALLOWED"));
        }
        // Compile a tiny portable child so argument quoting, stdin and process lifetime
        // exercise the actual platform process layer without requiring a project toolchain.
        let fixture = root.join("fixture.rs");
        std::fs::write(&fixture, include_str!("../fixtures/task-child.rs"))?;
        let runner = root.join(if cfg!(windows) {
            "fixture child.exe"
        } else {
            "fixture child"
        });
        let status = Command::new("rustc")
            .arg(&fixture)
            .arg("-o")
            .arg(&runner)
            .status()
            .await?;
        assert!(status.success());

        Ok(Self {
            root,
            source,
            target,
            observer,
            target_id,
            admin,
            invite,
            cfg,
            runner,
            target_daemon,
            _source_daemon,
            _observer_daemon,
            server,
            _temp: temp,
        })
    }
}

pub(super) async fn execution_transfer_and_identity() -> Result<()> {
    let mut suite = Suite::new().await?;
    environment::check(&suite).await?;
    let deduplicated_job = execution::check(&suite).await?;
    files::check(&suite).await?;
    access::check(&mut suite).await?;
    recovery::check(&mut suite).await?;
    membership::check(&mut suite, &deduplicated_job).await
}
#[cfg(target_os = "linux")]
pub(super) use service::foreground_shutdown;
