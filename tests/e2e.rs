use anyhow::{Context, Result};
use serde_json::Value;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::{Child, Command};
use xrun::{
    config::ServerConfig,
    crypto,
    protocol::*,
    store::{Invitation, ServerStore},
};

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}
fn command(home: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xrun"));
    cmd.env("HOME", home)
        .env("USERPROFILE", home)
        .args(args)
        .kill_on_drop(true);
    cmd
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
        command(home, &["daemon"])
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execution_transfer_and_identity() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let root = temp.path();
    let source = root.join("source");
    let target = root.join("target");
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
    let server = tokio::spawn(xrun::server::run(cfg.clone()));
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
    ok(cli(&source, &["join", &link, "--name", "admin", "--no-daemon"]).await);
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
    let invite = json(cli(&source, &["invite", "--json"]).await)["link"]
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
    let mut target_daemon = daemon(&target);
    online(&source, "runner1").await;
    online(&target, "admin").await;
    // Compile a tiny portable child so argument quoting, stdin and process lifetime
    // exercise the actual platform process layer without requiring a project toolchain.
    let fixture = root.join("fixture.rs");
    std::fs::write(
        &fixture,
        r#"use std::{io::{Read,Write},time::Duration};fn main(){let a:Vec<String>=std::env::args().skip(1).collect();match a[0].as_str(){"echo"=>{for s in &a[1..]{println!("<{s}>")}},"input"=>{let mut b=vec![];std::io::stdin().read_to_end(&mut b).unwrap();std::io::stdout().write_all(&b).unwrap();},"env"=>print!("{}",std::env::var("XRUN_TEST_SECRET").unwrap()),"sleep"=>std::thread::sleep(Duration::from_secs(30)),"exit"=>{eprintln!("error bytes");std::process::exit(7)},"detached"=>{let _=std::process::Command::new(std::env::current_exe().unwrap()).arg("sleep").spawn().unwrap();print!("parent done")},_=>panic!()}}"#,
    )?;
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
    let runner = runner.to_string_lossy();
    assert_eq!(
        ok(cli(
            &source,
            &[
                "runner1",
                "--",
                &runner,
                "echo",
                "hello world",
                "",
                "中文",
                "a\\\"b"
            ]
        )
        .await),
        "<hello world>\n<>\n<中文>\n<a\\\"b>\n"
    );
    let bytes = b"\0\xffhello\r\n";
    assert_eq!(
        input(
            &source,
            &["runner1", "--stdin", "--", &runner, "input"],
            bytes
        )
        .await
        .stdout,
        bytes
    );
    let exit = cli(&source, &["runner1", "--", &runner, "exit"]).await;
    assert_eq!(exit.status.code(), Some(7));
    assert_eq!(exit.stderr, b"error bytes\n");
    let timed = cli(
        &source,
        &["runner1", "--timeout", "1", "--", &runner, "sleep"],
    )
    .await;
    assert_eq!(timed.status.code(), Some(124));
    let first = json(
        cli(
            &source,
            &[
                "runner1",
                "start",
                "--request-id",
                "dedupe-1",
                "--env",
                "XRUN_TEST_SECRET=private-value",
                "--json",
                "--",
                &runner,
                "env",
            ],
        )
        .await,
    );
    let second = json(
        cli(
            &source,
            &[
                "runner1",
                "start",
                "--request-id",
                "dedupe-1",
                "--env",
                "XRUN_TEST_SECRET=private-value",
                "--json",
                "--",
                &runner,
                "env",
            ],
        )
        .await,
    );
    assert_eq!(first["job_id"], second["job_id"]);
    assert_eq!(
        cli(
            &source,
            &[
                "admin",
                "start",
                "--request-id",
                "dedupe-1",
                "--",
                &runner,
                "echo"
            ]
        )
        .await
        .status
        .code(),
        Some(2)
    );
    let conflict = cli(
        &source,
        &[
            "runner1",
            "start",
            "--request-id",
            "dedupe-1",
            "--json",
            "--",
            &runner,
            "echo",
        ],
    )
    .await;
    assert_eq!(conflict.status.code(), Some(2));
    let id = first["job_id"].as_str().unwrap();
    let waited = json(cli(&source, &["runner1", "wait", id, "--json"]).await);
    assert_eq!(waited["job"]["state"], "exited");
    // Expiring logs must retain the task result and original request deduplication.
    let tasks = xrun::store::TaskStore::open(&target.join(".xrun/daemon.db"), false)?;
    let mut expired = tasks.get(id)?.unwrap();
    expired.updated_at_ms = now_ms() - 8 * 86_400_000;
    tasks.save(&expired)?;
    tasks.prune()?;
    let unavailable = cli(&source, &["runner1", "logs", id]).await;
    assert_eq!(unavailable.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&unavailable.stderr).contains("LOG_UNAVAILABLE"));
    let waited = json(cli(&source, &["runner1", "wait", id, "--json"]).await);
    assert_eq!(waited["job"]["state"], "exited");
    assert!(
        waited["logs_error"]
            .as_str()
            .unwrap()
            .contains("LOG_UNAVAILABLE")
    );
    assert_eq!(tasks.by_request(&admin, "dedupe-1")?.unwrap().job_id, id);
    drop(tasks);
    let recent = json(cli(&source, &["recent", "--json"]).await);
    assert!(
        recent
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["request_id"] == "dedupe-1" && s["status"] == "confirmed")
    );
    #[cfg(unix)]
    for (signal, request, code, canceled) in [
        (libc::SIGTERM, "term-test", 75, false),
        (libc::SIGINT, "interrupt-test", 130, true),
    ] {
        let child = command(
            &source,
            &["runner1", "--request-id", request, "--", &runner, "sleep"],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
        let mut accepted = None;
        for _ in 0..100 {
            let jobs = json(
                cli(
                    &source,
                    &["runner1", "jobs", "--request-id", request, "--json"],
                )
                .await,
            );
            if let Some(job) = jobs
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["state"] == "running")
            {
                accepted = Some(job["job_id"].as_str().unwrap().to_string());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let accepted = accepted.context("signal test task never started")?;
        assert_eq!(unsafe { libc::kill(child.id().unwrap() as i32, signal) }, 0);
        let out = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output()).await??;
        assert_eq!(
            out.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let state = json(cli(&source, &["runner1", "jobs", &accepted, "--json"]).await);
        assert_eq!(
            state["state"],
            if canceled { "canceled" } else { "running" }
        );
        if !canceled {
            ok(cli(&source, &["runner1", "kill", &accepted]).await);
        }
    }
    let detached = cli(&source, &["runner1", "--", &runner, "detached"]).await;
    assert_eq!(detached.status.code(), Some(0));
    assert_eq!(detached.stdout, b"parent done");
    #[cfg(unix)]
    assert_eq!(
        ok(input(
            &source,
            &["runner1", "--script", "sh"],
            b"printf 'script ok'"
        )
        .await),
        "script ok"
    );
    #[cfg(windows)]
    for script in [
        b"Write-Output 'script ok'\n".as_slice(),
        b"\xef\xbb\xbfWrite-Output 'script ok'\n".as_slice(),
    ] {
        assert_eq!(
            ok(input(&source, &["runner1", "--script", "powershell"], script).await),
            "script ok\r\n"
        );
    }
    // Capacity rejection does not create a job or poison the request ID.
    let mut jobs = vec![];
    for _ in 0..4 {
        jobs.push(
            ok(cli(&source, &["runner1", "start", "--", &runner, "sleep"]).await)
                .trim()
                .to_string(),
        );
    }
    let busy = cli(
        &source,
        &[
            "runner1",
            "start",
            "--request-id",
            "busy-retry",
            "--",
            &runner,
            "echo",
            "retry",
        ],
    )
    .await;
    assert_eq!(busy.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&busy.stderr).contains("DEVICE_BUSY"));
    assert_eq!(
        json(
            cli(
                &source,
                &["runner1", "jobs", "--request-id", "busy-retry", "--json"]
            )
            .await
        ),
        serde_json::json!([])
    );
    for job in jobs {
        ok(cli(&source, &["runner1", "kill", &job]).await);
    }
    let retry = cli(
        &source,
        &[
            "runner1",
            "start",
            "--request-id",
            "busy-retry",
            "--",
            &runner,
            "echo",
            "retry",
        ],
    )
    .await;
    ok(retry);
    // Larger than one WebSocket message, with BOM, CRLF and arbitrary bytes.
    let mut content = b"\xef\xbb\xbfhello\r\n".to_vec();
    content.extend((0..2_100_000).map(|i| (i % 256) as u8));
    let remote = target.join("artifact.bin");
    let remote = remote.to_string_lossy();
    ok(input(&source, &["runner1", "push", "-", &remote], &content).await);
    let local = source.join("download.bin");
    let local = local.to_string_lossy();
    let pulled = json(cli(&source, &["runner1", "pull", &remote, &local, "--json"]).await);
    assert_eq!(pulled["sha256"], sha256(&content));
    assert_eq!(std::fs::read(&*local)?, content);
    let default = json(cli(&source, &["runner1", "pull", &remote, "--json"]).await);
    let temporary = std::path::PathBuf::from(default["path"].as_str().unwrap());
    assert!(
        temporary
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("xrun-pull-")
    );
    assert_eq!(std::fs::read(&temporary)?, content);
    std::fs::remove_file(temporary)?;
    let stale = input(
        &source,
        &["runner1", "push", "-", &remote, "--expect", &"0".repeat(64)],
        b"changed",
    )
    .await;
    assert_eq!(stale.status.code(), Some(1));
    assert_eq!(std::fs::read(&*remote)?, content);
    ok(input(
        &source,
        &[
            "runner1",
            "push",
            "-",
            &remote,
            "--expect",
            &sha256(&content),
        ],
        b"changed",
    )
    .await);
    assert_eq!(
        cli(&source, &["runner1", "pull", &remote, "-"])
            .await
            .stdout,
        b"changed"
    );
    assert_eq!(
        cli(&source, &["runner1", "pull", &remote, "-", "--json"])
            .await
            .status
            .code(),
        Some(2)
    );
    let exists = input(
        &source,
        &["runner1", "push", "-", &remote, "--no-overwrite"],
        b"bad",
    )
    .await;
    assert_eq!(exists.status.code(), Some(1));
    assert_eq!(std::fs::read(&*remote)?, b"changed");
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        std::fs::set_permissions(&*remote, std::fs::Permissions::from_mode(0o640))?;
        let link = target.join("link.bin");
        symlink(&*remote, &link)?;
        ok(input(
            &source,
            &["runner1", "push", "-", &link.to_string_lossy()],
            b"via link",
        )
        .await);
        assert!(std::fs::symlink_metadata(&link)?.file_type().is_symlink());
        assert_eq!(
            std::fs::metadata(&*remote)?.permissions().mode() & 0o777,
            0o640
        );
    }
    // Whitelist changes are read by the daemon for subsequent requests.
    ok(cli(&target, &["deny-from", &admin]).await);
    ok(cli(&target, &["join", &invite, "--no-daemon"]).await);
    let denied = cli(&source, &["runner1", "--", &runner, "echo"]).await;
    assert_eq!(denied.status.code(), Some(125));
    ok(cli(&target, &["allow-from", &admin]).await);
    online(&source, "runner1").await;
    let id_before = std::fs::read_to_string(target.join(".xrun/identity.toml"))?;
    ok(cli(&target, &["join", &invite, "--no-daemon"]).await);
    let id_after = std::fs::read_to_string(target.join(".xrun/identity.toml"))?;
    assert!(id_before.contains(&target_id) && id_after.contains(&target_id));
    // A crash never replays the intent. The stored PID/start proof cleans up the
    // old process, and its result becomes lost without consuming new capacity.
    let crashed = json(
        cli(
            &source,
            &[
                "runner1",
                "start",
                "--request-id",
                "crash-test",
                "--json",
                "--",
                &runner,
                "sleep",
            ],
        )
        .await,
    );
    let crash_id = crashed["job_id"].as_str().unwrap();
    for _ in 0..100 {
        if json(cli(&source, &["runner1", "jobs", crash_id, "--json"]).await)["state"] == "running"
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    target_daemon.0.start_kill()?;
    target_daemon.0.wait().await?;
    target_daemon = daemon(&target);
    online(&source, "runner1").await;
    let lost = json(cli(&source, &["runner1", "jobs", crash_id, "--json"]).await);
    assert_eq!(lost["state"], "lost");
    assert_eq!(
        cli(&source, &["runner1", "wait", crash_id])
            .await
            .status
            .code(),
        Some(125)
    );
    let dedup_lost = json(
        cli(
            &source,
            &[
                "runner1",
                "start",
                "--request-id",
                "crash-test",
                "--json",
                "--",
                &runner,
                "sleep",
            ],
        )
        .await,
    );
    assert_eq!(dedup_lost["job_id"], crash_id);
    // Server must have no task/log tables or execution secrets.
    let db = rusqlite::Connection::open(cfg.data_dir.join("server.db"))?;
    let forbidden:i64=db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN ('jobs','logs','submissions')",[],|r|r.get(0))?;
    assert_eq!(forbidden, 0);
    let task_db = rusqlite::Connection::open(target.join(".xrun/daemon.db"))?;
    let metadata: String = task_db.query_row(
        "SELECT data FROM jobs WHERE request_id='dedupe-1'",
        [],
        |r| r.get(0),
    )?;
    assert!(!metadata.contains("private-value"));
    // Reset is explicit and refuses to operate while a daemon is running.
    assert_eq!(
        cli(&target, &["daemon", "reset"]).await.status.code(),
        Some(125)
    );
    target_daemon.0.start_kill()?;
    target_daemon.0.wait().await?;
    let db_path = target.join(".xrun/daemon.db");
    drop(task_db);
    std::fs::rename(&db_path, target.join("old-daemon.db"))?;
    assert_eq!(cli(&target, &["daemon"]).await.status.code(), Some(125));
    ok(cli(&target, &["daemon", "reset"]).await);
    target_daemon = daemon(&target);
    online(&source, "runner1").await;
    let reset = cli(
        &source,
        &[
            "runner1",
            "start",
            "--request-id",
            "crash-test",
            "--json",
            "--",
            &runner,
            "sleep",
        ],
    )
    .await;
    assert_eq!(reset.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&reset.stderr).contains("DB_RESET"));
    assert_eq!(
        json(cli(&source, &["runner1", "jobs", "--json"]).await),
        serde_json::json!([])
    );
    // Revoked identities retain distinct errors and cannot renew using their key.
    let not_admin = cli(&target, &["revoke", "admin"]).await;
    assert_eq!(not_admin.status.code(), Some(125));
    ok(cli(&source, &["revoke", "runner1"]).await);
    let revoked = cli(&source, &["runner1", "jobs", id]).await;
    assert_eq!(revoked.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&revoked.stderr).contains("DEVICE_REVOKED"));
    let rejected = cli(&target, &["join", &invite, "--no-daemon"]).await;
    assert_eq!(rejected.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("DEVICE_REVOKED"));
    target_daemon.0.start_kill()?;
    server.abort();
    let _ = server.await;
    let unreachable = cli(&source, &["status", "--json"]).await;
    assert_eq!(unreachable.status.code(), Some(125));
    let status: Value = serde_json::from_slice(&unreachable.stdout)?;
    assert_eq!(status["local"]["joined"], true);
    assert!(status["devices"].is_null());
    assert!(!status["server_error"].is_null());
    Ok(())
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn linux_foreground_deployment_and_admin_recovery() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let temp = tempfile::tempdir()?;
    let home = temp.path();
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port()
        .to_string();
    let address = format!("127.0.0.1:{port}");
    let mut old_id = None;
    let mut ca = None;
    for generation in 0..3 {
        if generation == 2 {
            std::fs::remove_file(home.join(".xrun/identity.toml"))?;
        }
        let args = if generation == 0 {
            vec![
                "up",
                "--port",
                &port,
                "--addr",
                &address,
                "--no-service",
                "--no-daemon",
                "--json",
            ]
        } else {
            vec!["up", "--no-service", "--no-daemon", "--json"]
        };
        let mut child = command(home, &args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(15), output.read_line(&mut line)).await??;
        assert!(!line.is_empty(), "up exited before deployment output");
        let value: Value = serde_json::from_str(&line)?;
        let id = value["device_id"].as_str().unwrap().to_string();
        assert_eq!(value["addresses"], serde_json::json!([address]));
        if generation == 1 {
            assert_eq!(old_id.as_deref(), Some(id.as_str()))
        }
        if generation == 2 {
            assert_ne!(old_id.as_deref(), Some(id.as_str()))
        }
        old_id = Some(id);
        let current_ca = std::fs::read(home.join(".xrun/server/ca.pem"))?;
        if let Some(previous) = &ca {
            assert_eq!(&current_ca, previous)
        }
        ca = Some(current_ca);
        let status = json(cli(home, &["status", "--json"]).await);
        assert_eq!(status["local"]["device_id"], value["device_id"]);
        let changed = cli(home, &["up", "--port", "1", "--no-service", "--no-daemon"]).await;
        assert_eq!(changed.status.code(), Some(125));
        unsafe {
            libc::kill(child.id().unwrap() as i32, libc::SIGTERM);
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await??
                .success()
        );
    }
    Ok(())
}
