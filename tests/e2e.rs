mod common;
use anyhow::{Context, Result};
use serde_json::Value;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::process::{Child, Command};
use xrun::{config::ServerConfig, protocol::*};

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn execution_transfer_and_identity() -> Result<()> {
    common::library_logs()?;
    let temp = tempfile::tempdir()?;
    let root = temp.path();
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
    let link = xrun::relay::deployment_link(&cfg)?;
    let server = tokio::spawn(xrun::relay::run(cfg.clone()));
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
    let build_env = [
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
    let mut target_command = common::logged(&target, &["daemon"], "target")?;
    for name in build_env {
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
    let mut target_daemon = Daemon(target_command.spawn()?);
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
    std::fs::write(
        &fixture,
        r#"use std::{io::{Read, Write}, time::Duration};
fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a[0].as_str() {
        "echo" => { for s in &a[1..] { println!("<{s}>"); } },
        "input" => {
            let mut b = vec![];
            std::io::stdin().read_to_end(&mut b).unwrap();
            std::io::stdout().write_all(&b).unwrap();
        },
        "env" => print!("{}", std::env::var("XRUN_TEST_SECRET").unwrap()),
        "env-values" => {
            for name in &a[1..] {
                println!("{name}={}", std::env::var(name).unwrap_or_default());
            }
        },
        "argv0" => print!("{}", std::path::Path::new(&std::env::args().next().unwrap())
            .file_name().unwrap().to_string_lossy()),
        "sleep" => std::thread::sleep(Duration::from_secs(30)),
        "finish-later" => { std::thread::sleep(Duration::from_secs(1)); print!("completed"); },
        "exit" => { eprintln!("error bytes"); std::process::exit(7); },
        "detached" => {
            let _ = std::process::Command::new(std::env::current_exe().unwrap()).arg("sleep").spawn().unwrap();
            print!("parent done");
        },
        _ => panic!(),
    }
}"#,
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
    let mut env_args = vec!["runner1", "--", &runner, "env-values"];
    env_args.extend(build_env);
    env_args.extend(["XRUN_ENV_PRESERVED", "CARGO_HOME", "RUSTUP_HOME", "PATH"]);
    let values = ok(cli(&source, &env_args).await);
    let values: std::collections::BTreeMap<_, _> = values
        .lines()
        .map(|line| line.split_once('=').unwrap())
        .collect();
    for name in build_env {
        assert_eq!(values[name], "", "inherited {name} leaked into the task");
    }
    assert_eq!(values["XRUN_ENV_PRESERVED"], "keep");
    assert_eq!(values["CARGO_HOME"], "cargo-home-kept");
    assert_eq!(values["RUSTUP_HOME"], "rustup-home-kept");
    assert!(!values["PATH"].is_empty());

    // Config and per-request values intentionally restore filtered variables;
    // a request must still override the configured value.
    let config_path = target.join(".xrun/daemon.toml");
    let mut task_config: xrun::config::DaemonConfig = xrun::config::read(&config_path)?;
    task_config
        .env
        .insert("CARGO_TARGET_DIR".into(), "configured-target".into());
    task_config
        .env
        .insert("RUSTUP_TOOLCHAIN".into(), "configured-toolchain".into());
    xrun::config::write(&config_path, &task_config)?;
    assert_eq!(
        ok(cli(
            &source,
            &[
                "runner1",
                "--",
                &runner,
                "env-values",
                "CARGO_TARGET_DIR",
                "RUSTUP_TOOLCHAIN",
            ]
        )
        .await),
        "CARGO_TARGET_DIR=configured-target\nRUSTUP_TOOLCHAIN=configured-toolchain\n"
    );
    assert_eq!(
        ok(cli(
            &source,
            &[
                "runner1",
                "--env",
                "CARGO_TARGET_DIR=requested-target",
                "--env",
                if cfg!(windows) {
                    "rustup_toolchain=requested-toolchain"
                } else {
                    "RUSTUP_TOOLCHAIN=requested-toolchain"
                },
                "--",
                &runner,
                "env-values",
                "CARGO_TARGET_DIR",
                "RUSTUP_TOOLCHAIN",
            ]
        )
        .await),
        "CARGO_TARGET_DIR=requested-target\nRUSTUP_TOOLCHAIN=requested-toolchain\n"
    );
    task_config.env.remove("CARGO_TARGET_DIR");
    task_config.env.remove("RUSTUP_TOOLCHAIN");
    xrun::config::write(&config_path, &task_config)?;

    #[cfg(unix)]
    {
        let link = root.join("cargo");
        std::os::unix::fs::symlink(runner.as_ref(), &link)?;
        let link = link.to_string_lossy();
        let cwd = root.to_string_lossy();
        let path = format!("PATH={cwd}");
        for args in [
            vec!["runner1", "--", &link, "argv0"],
            vec!["runner1", "--env", &path, "--", "cargo", "argv0"],
            vec!["runner1", "-C", &cwd, "--", "./cargo", "argv0"],
            vec![
                "runner1", "-C", &cwd, "--env", "PATH=.", "--", "cargo", "argv0",
            ],
        ] {
            assert_eq!(ok(cli(&source, &args).await), "cargo");
        }
    }
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
    // JSON diagnostics retain their documented code as well as exit status.
    let expired_logs = tasks.get(id)?.unwrap();
    for (reason, code) in [
        ("LOG_EXPIRED", "LOG_UNAVAILABLE"),
        ("TRUNCATED", "LOG_TRUNCATED"),
        ("CAPTURE_ERROR: disk unavailable", "LOG_INCOMPLETE"),
    ] {
        let mut partial = expired_logs.clone();
        partial.incomplete_reason = Some(reason.into());
        tasks.save(&partial)?;
        let result = cli(&source, &["runner1", "logs", id, "--json"]).await;
        assert_eq!(result.status.code(), Some(1));
        let diagnostic: Value = serde_json::from_slice(&result.stderr)?;
        assert_eq!(diagnostic["code"], code);
        assert_eq!(diagnostic["message"], reason);
    }
    tasks.save(&expired_logs)?;
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
    #[cfg(windows)]
    {
        // cmd paths and arguments must survive its own parser, not CRT quoting.
        let argument = "space & caret^ percent%XRUN_PATH_TEST% bang!";
        assert_eq!(
            ok(input(
                &source,
                &["runner1", "--script", "cmd", "--", argument],
                b"@echo off\necho \"%~1\"\n"
            )
            .await),
            format!("\"{argument}\"\r\n")
        );
        let invalid = input(
            &source,
            &["runner1", "--script", "cmd", "--", "a\"b"],
            b"echo unused\n",
        )
        .await;
        assert_eq!(invalid.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&invalid.stderr).contains("INVALID_SCRIPT_ARGUMENT"));
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
    #[cfg(unix)]
    {
        let victim = source.join("victim");
        let link = source.join("output-link");
        std::fs::write(&victim, b"keep")?;
        std::os::unix::fs::symlink(&victim, &link)?;
        let refused = cli(
            &source,
            &["runner1", "pull", &remote, link.to_str().unwrap()],
        )
        .await;
        assert_eq!(refused.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&refused.stderr).contains("INVALID_PATH"));
        assert_eq!(std::fs::read(&victim)?, b"keep");
    }
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
    assert_eq!(
        ok(cli(&observer, &["runner1", "--", &runner, "echo", "future"]).await),
        "<future>\n"
    );
    ok(cli(&target, &["deny-from", "observer"]).await);
    let refused = cli(&observer, &["runner1", "--", &runner, "echo"]).await;
    assert!(String::from_utf8_lossy(&refused.stderr).contains("SOURCE_NOT_ALLOWED"));
    ok(cli(&target, &["deny-from", "--all"]).await);
    // Turning off all-member mode preserves the admin's individual grant.
    assert_eq!(
        ok(cli(&source, &["runner1", "--", &runner, "echo", "known"]).await),
        "<known>\n"
    );
    assert_eq!(
        cli(&target, &["allow-from", "observer", "--all"])
            .await
            .status
            .code(),
        Some(2)
    );

    let completed = json(
        cli(
            &source,
            &["runner1", "start", "--json", "--", &runner, "finish-later"],
        )
        .await,
    );
    let completed_id = completed["job_id"].as_str().unwrap();
    let paused_job = json(
        cli(
            &source,
            &["runner1", "start", "--json", "--", &runner, "sleep"],
        )
        .await,
    );
    let paused_job_id = paused_job["job_id"].as_str().unwrap();
    let source_identity = xrun::config::read(&source.join(".xrun/identity.toml"))?;
    let mut subscription = common::peer_session(&source, &source_identity, &target_id).await?;
    assert!(matches!(
        xrun::net::receive::<Data>(&mut subscription).await?,
        Data::Ready { .. }
    ));
    xrun::net::send(
        &mut subscription,
        &Data::Request {
            request: Request::Logs {
                id: paused_job_id.into(),
                after: 0,
                follow: true,
            },
        },
    )
    .await?;
    assert!(matches!(
        xrun::net::receive::<Data>(&mut subscription).await?,
        Data::Logs { .. }
    ));
    ok(cli(&target, &["daemon", "pause"]).await);
    let refused = cli(&source, &["runner1", "jobs", "--json"]).await;
    assert_eq!(refused.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("ACCESS_PAUSED"));
    // Even a fast resume invalidates the subscription opened before the pause.
    ok(cli(&target, &["daemon", "resume"]).await);
    // Drain in-flight logs and the optional denial response before checking
    // transport closure. Neither a diagnostic nor a timeout proves closure.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match xrun::net::receive::<Data>(&mut subscription).await {
                Ok(Data::Logs { job, .. }) if job.job_id == paused_job_id => {}
                Ok(Data::Error { code, .. })
                    if matches!(code.as_str(), "ACCESS_PAUSED" | "SESSION_CLOSED") => {}
                Ok(message) => anyhow::bail!("unexpected message after pause: {message:?}"),
                Err(_) => return Ok::<_, anyhow::Error>(()),
            }
        }
    })
    .await
    .context("paused log subscription remained open after resume")??;
    ok(cli(&target, &["daemon", "pause"]).await);
    let local_tasks = xrun::store::TaskStore::open(&target.join(".xrun/daemon.db"), false)?;
    assert!(!local_tasks.get(paused_job_id)?.unwrap().state.terminal());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if local_tasks.get(completed_id).unwrap().unwrap().state == JobState::Exited {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    // Pause survives restarting the daemon and does not erase task history.
    ok(cli(&target, &["daemon", "stop"]).await);
    assert!(
        tokio::time::timeout(Duration::from_secs(12), target_daemon.0.wait())
            .await??
            .success()
    );
    target_daemon = daemon(&target);
    online(&source, "runner1").await;
    assert_eq!(
        json(cli(&target, &["status", "--json"]).await)["local"]["remote_access_paused"],
        true
    );
    assert_eq!(
        cli(&source, &["runner1", "jobs", "--json"])
            .await
            .status
            .code(),
        Some(125)
    );
    ok(cli(&target, &["daemon", "resume"]).await);
    assert_eq!(
        json(cli(&source, &["runner1", "jobs", completed_id, "--json"]).await)["state"],
        "exited"
    );
    drop(local_tasks);
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
    // Desktop Stop must cancel jobs, record the outcome and exit successfully,
    // so supervisors do not immediately relaunch the daemon.
    let graceful = json(
        cli(
            &source,
            &["runner1", "start", "--json", "--", &runner, "sleep"],
        )
        .await,
    );
    let graceful_id = graceful["job_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if json(cli(&source, &["runner1", "jobs", graceful_id, "--json"]).await)["state"]
                == "running"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    assert_eq!(
        json(cli(&target, &["status", "--json"]).await)["local"]["daemon_connected"],
        true
    );
    ok(cli(&target, &["daemon", "stop"]).await);
    assert!(
        tokio::time::timeout(Duration::from_secs(12), target_daemon.0.wait())
            .await??
            .success()
    );
    assert!(!xrun::config::instance_running(
        &target.join(".xrun/daemon.lock")
    )?);
    target_daemon = daemon(&target);
    online(&source, "runner1").await;
    assert_eq!(
        json(cli(&source, &["runner1", "jobs", graceful_id, "--json"]).await)["state"],
        "canceled"
    );
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
    let db = rusqlite::Connection::open(cfg.data_dir.join("relay.db"))?;
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
    let denied_invite = cli(&target, &["invite", "--json"]).await;
    assert_eq!(denied_invite.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&denied_invite.stderr).contains("NOT_MANAGER"));
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
async fn linux_relay_service_configuration_and_foreground_shutdown() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path();
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let cfg = ServerConfig {
        port,
        addresses: vec![format!("127.0.0.1:{port}")],
        manual: true,
        no_detect: true,
        data_dir: home.join(".xrun/server"),
    };
    std::fs::create_dir_all(home.join(".xrun"))?;
    xrun::config::write(&home.join(".xrun/config.toml"), &cfg)?;
    let link = xrun::relay::deployment_link(&cfg)?;
    let mut child = common::logged(home, &["relay", "run"], "relay")?.spawn()?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            assert!(child.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    ok(cli(home, &["up", "--relay", &link, "--no-daemon", "--json"]).await);
    let id: xrun::config::Identity = xrun::config::read(&home.join(".xrun/identity.toml"))?;
    // Re-running up retains the manager identity and signing authority.
    ok(cli(home, &["up", "--relay", &link, "--no-daemon", "--json"]).await);
    let same: xrun::config::Identity = xrun::config::read(&home.join(".xrun/identity.toml"))?;
    assert_eq!(id.device_id, same.device_id);
    assert_eq!(id.ca_pem, same.ca_pem);
    unsafe {
        libc::kill(child.id().unwrap() as i32, libc::SIGTERM);
    }
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await??
            .success()
    );
    // Missing manager identity must not silently recreate an authority.
    std::fs::remove_file(home.join(".xrun/identity.toml"))?;
    let rejected = cli(home, &["up", "--relay", &link, "--no-daemon"]).await;
    assert_eq!(rejected.status.code(), Some(125));
    Ok(())
}
