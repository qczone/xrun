use std::{
    fs,
    io::Write,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn xrun(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_xrun"))
        .env("HOME", home)
        .args(args)
        .output()
        .unwrap()
}

fn success(out: std::process::Output) -> String {
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn submitted_job(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .find_map(|line| line.strip_prefix("[xrun] job_id="))
        .expect("job ID in CLI status")
        .to_owned()
}

#[test]
fn pair_route_and_execute() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let source = root.join("source");
    let target = root.join("target");
    let server_dir = root.join("server");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&target).unwrap();
    fs::create_dir_all(&server_dir).unwrap();
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = root.join("server.toml");
    fs::write(&config,format!("listen = \"127.0.0.1:{port}\"\npublic_url = \"https://127.0.0.1:{port}\"\ndata_dir = \"{}\"\n",server_dir.display())).unwrap();
    let server = Command::new(env!("CARGO_BIN_EXE_xrun"))
        .args(["server", "--config", config.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server_guard = ChildGuard(server);
    let until = Instant::now() + Duration::from_secs(10);
    while !server_dir.join("admin.sock").exists() {
        assert!(Instant::now() < until, "admin socket not created");
        thread::sleep(Duration::from_millis(50));
    }
    let link = success(xrun(
        &source,
        &["pair", "--config", config.to_str().unwrap()],
    ))
    .trim()
    .to_owned();
    let bad_home = root.join("bad");
    fs::create_dir_all(&bad_home).unwrap();
    let tampered = link.replace("ca=", "ca=00");
    let rejected = xrun(&bad_home, &["join", &tampered, "--name", "bad1"]);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("fingerprint mismatch"));
    let joined = success(xrun(&source, &["join", &link, "--name", "source1"]));
    success(xrun(&source, &["join", &link, "--name", "source1"]));
    let source_id = joined.split_whitespace().last().unwrap().to_owned();
    let link = success(xrun(
        &source,
        &["pair", "--config", config.to_str().unwrap()],
    ))
    .trim()
    .to_owned();
    success(xrun(&target, &["join", &link, "--name", "target1"]));
    fs::write(
        target.join(".xrun/agent.toml"),
        format!("allow_from = [\"{source_id}\"]\n"),
    )
    .unwrap();
    let agent = Command::new(env!("CARGO_BIN_EXE_xrun"))
        .env("HOME", &target)
        .arg("agent")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut agent_guard = ChildGuard(agent);
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let out = xrun(&source, &["ls"]);
        if out.status.success()
            && String::from_utf8_lossy(&out.stdout).contains("target1")
            && String::from_utf8_lossy(&out.stdout).contains("online")
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "target Agent did not come online: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        thread::sleep(Duration::from_millis(100));
    }
    let before: serde_json::Value =
        serde_json::from_str(&success(xrun(&source, &["--json", "jobs"]))).unwrap();
    let mut oversized = Command::new(env!("CARGO_BIN_EXE_xrun"))
        .env("HOME", &source)
        .args(["exec", "target1", "--stdin", "--", "/bin/cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    oversized
        .stdin
        .take()
        .unwrap()
        .write_all(&vec![b'x'; 1024 * 1024 + 1])
        .unwrap();
    let oversized = oversized.wait_with_output().unwrap();
    assert_eq!(oversized.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&oversized.stderr).contains("STDIN_TOO_LARGE"));
    let after: serde_json::Value =
        serde_json::from_str(&success(xrun(&source, &["--json", "jobs"]))).unwrap();
    assert_eq!(
        before.as_array().unwrap().len(),
        after.as_array().unwrap().len()
    );
    let out = xrun(
        &source,
        &[
            "exec",
            "target1",
            "-C",
            target.to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "printf hello",
        ],
    );
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"hello");
    let small_id = submitted_job(&out.stderr);

    let large = xrun(
        &source,
        &[
            "exec",
            "target1",
            "--",
            "/bin/sh",
            "-c",
            "head -c 700000 /dev/zero",
        ],
    );
    assert!(
        large.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&large.stderr)
    );
    assert_eq!(large.stdout.len(), 700000);
    assert!(large.stdout.iter().all(|b| *b == 0));
    let large_id = submitted_job(&large.stderr);

    let mut child = Command::new(env!("CARGO_BIN_EXE_xrun"))
        .env("HOME", &source)
        .args(["exec", "target1", "--stdin", "--", "/bin/cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"\0\xff\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"\0\xff\n");

    let mut early_close = Command::new(env!("CARGO_BIN_EXE_xrun"))
        .env("HOME", &source)
        .args(["exec", "target1", "--stdin", "--", "/usr/bin/true"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    early_close
        .stdin
        .take()
        .unwrap()
        .write_all(&vec![0; 1024 * 1024])
        .unwrap();
    let out = early_close.wait_with_output().unwrap();
    let early_job = success(xrun(
        &source,
        &["--json", "job", &submitted_job(&out.stderr)],
    ));
    assert!(
        out.status.success(),
        "early stdin close should preserve process result: {}; job={early_job}",
        String::from_utf8_lossy(&out.stderr),
    );

    let key = "e2e-request-id";
    let out = success(xrun(
        &source,
        &[
            "--json",
            "exec",
            "target1",
            "--request-id",
            key,
            "--detach",
            "--env",
            "XRUN_SECRET=do-not-store-this",
            "--",
            "/bin/true",
        ],
    ));
    let first: serde_json::Value = serde_json::from_str(out.lines().nth(1).unwrap()).unwrap();
    let job_id = first["job"]["job_id"].as_str().unwrap();
    let out = success(xrun(
        &source,
        &[
            "--json",
            "exec",
            "target1",
            "--request-id",
            key,
            "--detach",
            "--env",
            "XRUN_SECRET=do-not-store-this",
            "--",
            "/bin/true",
        ],
    ));
    let second: serde_json::Value = serde_json::from_str(out.lines().nth(1).unwrap()).unwrap();
    assert_eq!(job_id, second["job"]["job_id"]);
    let out = xrun(
        &source,
        &[
            "exec",
            "target1",
            "--request-id",
            key,
            "--detach",
            "--env",
            "XRUN_SECRET=changed",
            "--",
            "/bin/true",
        ],
    );
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("REQUEST_CONFLICT"));
    for path in [
        server_dir.join("server.sqlite"),
        server_dir.join("server.sqlite-wal"),
        target.join(".xrun/agent.sqlite"),
        target.join(".xrun/agent.sqlite-wal"),
    ]
    .into_iter()
    .filter(|p| p.exists())
    {
        let raw = fs::read(path).unwrap();
        assert!(
            !raw.windows(b"do-not-store-this".len())
                .any(|v| v == b"do-not-store-this")
        );
    }

    success(xrun(&source, &["renew"]));
    success(xrun(&source, &["info", "target1"]));
    let recovery = success(xrun(
        &source,
        &[
            "pair",
            "--config",
            config.to_str().unwrap(),
            "--renew",
            "target1",
        ],
    ));
    success(xrun(&target, &["join", recovery.trim()]));
    success(xrun(&source, &["info", "target1"]));

    let stranger = root.join("stranger");
    fs::create_dir_all(&stranger).unwrap();
    let link = success(xrun(
        &source,
        &["pair", "--config", config.to_str().unwrap()],
    ));
    success(xrun(
        &stranger,
        &["join", link.trim(), "--name", "stranger1"],
    ));
    let denied = xrun(&stranger, &["exec", "target1", "--", "/bin/true"]);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("SOURCE_NOT_ALLOWED"));
    for args in [
        vec!["job", small_id.as_str()],
        vec!["kill", small_id.as_str()],
        vec!["logs", small_id.as_str()],
    ] {
        let denied = xrun(&stranger, &args);
        assert!(!denied.status.success());
    }
    success(xrun(
        &source,
        &["revoke", "--config", config.to_str().unwrap(), "stranger1"],
    ));
    let revoked = xrun(&stranger, &["ls"]);
    assert!(!revoked.status.success());

    let out = xrun(
        &source,
        &[
            "--json",
            "exec",
            "target1",
            "--timeout",
            "1",
            "--",
            "/bin/sleep",
            "20",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(125),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("timed_out"));

    let out = success(xrun(
        &source,
        &[
            "--json",
            "exec",
            "target1",
            "--detach",
            "--",
            "/bin/sleep",
            "20",
        ],
    ));
    let accepted: serde_json::Value = serde_json::from_str(out.lines().nth(1).unwrap()).unwrap();
    let id = accepted["job"]["job_id"].as_str().unwrap();
    success(xrun(&source, &["kill", id]));
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let out = success(xrun(&source, &["--json", "job", id]));
        let j: serde_json::Value = serde_json::from_str(&out).unwrap();
        if j["state"] == "canceled" {
            break;
        }
        assert!(Instant::now() < until, "cancel not confirmed: {out}");
        thread::sleep(Duration::from_millis(100));
    }

    let out = success(xrun(
        &source,
        &[
            "--json",
            "exec",
            "target1",
            "-C",
            target.to_str().unwrap(),
            "--detach",
            "--",
            "/bin/sh",
            "-c",
            "printf once >> once.txt; sleep 4",
        ],
    ));
    let accepted: serde_json::Value = serde_json::from_str(out.lines().nth(1).unwrap()).unwrap();
    let running_id = accepted["job"]["job_id"].as_str().unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let out = success(xrun(&source, &["--json", "job", running_id]));
        let j: serde_json::Value = serde_json::from_str(&out).unwrap();
        if j["state"] == "running" {
            break;
        }
        assert!(Instant::now() < until, "job did not start: {out}");
        thread::sleep(Duration::from_millis(100));
    }
    server_guard.0.kill().unwrap();
    server_guard.0.wait().unwrap();
    thread::sleep(Duration::from_secs(5));
    server_guard.0 = Command::new(env!("CARGO_BIN_EXE_xrun"))
        .args(["server", "--config", config.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let out = xrun(&source, &["--json", "job", running_id]);
        if out.status.success() {
            let j: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            if j["state"] == "exited" {
                assert_eq!(j["exit_code"], 0);
                break;
            }
        }
        assert!(
            Instant::now() < until,
            "job was not reconciled after Server restart: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        thread::sleep(Duration::from_millis(200));
    }
    assert_eq!(fs::read(target.join("once.txt")).unwrap(), b"once");
    agent_guard.0.kill().unwrap();
    agent_guard.0.wait().unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let out = success(xrun(&source, &["ls"]));
        if out.contains("target1") && out.contains("offline") {
            break;
        }
        assert!(Instant::now() < until, "Agent did not go offline");
        thread::sleep(Duration::from_millis(100));
    }
    let cached = success(xrun(&source, &["logs", &small_id]));
    assert_eq!(cached, "hello");
    let partial = xrun(&source, &["logs", &large_id]);
    assert_eq!(partial.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&partial.stderr).contains("LOG_UNAVAILABLE"));
}
