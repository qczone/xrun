mod common;
use anyhow::{Context, Result};
use common::*;
use std::{path::Path, time::Duration};
use xrun::testing::{net, protocol::*, store::TaskStore};

fn store(lab: &Lab) -> Result<TaskStore> {
    TaskStore::open(&lab.target.join(".xrun/daemon.db"), false)
}
fn database(lab: &Lab) -> Result<rusqlite::Connection> {
    let db = rusqlite::Connection::open(lab.target.join(".xrun/daemon.db"))?;
    db.busy_timeout(Duration::from_secs(5))?;
    Ok(db)
}
fn execution(lab: &Lab, program: &Path, args: &[&str]) -> Result<Execution> {
    Ok(Execution {
        request_id: uuid::Uuid::new_v4().to_string(),
        db_id: store(lab)?.db_id,
        program: program.to_string_lossy().into(),
        args: args.iter().map(|v| (*v).into()).collect(),
        cwd: lab.target.to_string_lossy().into(),
        env: Default::default(),
        timeout: 30,
        shell: None,
        input_size: 0,
        input_sha256: sha256(&[]),
    })
}

// Bypass CLI validation but retain real relay authentication, TLS and daemon dispatch.
async fn submit(lab: &Lab, request: &Execution, input: &[u8]) -> Result<Data> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut ws = peer_session(
            &lab.source,
            &lab.source_identity,
            &lab.target_identity.device_id,
        )
        .await?;
        assert!(matches!(net::receive(&mut ws).await?, Data::Ready { .. }));
        net::send(
            &mut ws,
            &Data::Request {
                request: Request::Exec {
                    execution: request.clone(),
                    follow: false,
                },
            },
        )
        .await?;
        net::send_bytes(&mut ws, input).await?;
        let response: Data = net::receive(&mut ws).await?;
        if matches!(response, Data::Job { .. }) {
            assert!(matches!(net::receive(&mut ws).await?, Data::Complete));
        }
        let _ = ws.close(None).await;
        Ok::<_, anyhow::Error>(response)
    })
    .await?
}
fn accepted(data: Data) -> Job {
    match data {
        Data::Job { job } => job,
        other => panic!("expected accepted job, got {other:?}"),
    }
}
fn rejected(data: Data, expected: &str) {
    match data {
        Data::Error { code, .. } => assert_eq!(code, expected),
        other => panic!("expected {expected}, got {other:?}"),
    }
}
async fn terminal(lab: &Lab, id: &str) -> Result<Job> {
    let tasks = store(lab)?;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let job = tasks.get(id)?.context("accepted job disappeared")?;
            if job.state.terminal() {
                return Ok(job);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}
async fn pid_file(path: &Path) -> Result<u32> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Ok(pid) = text.parse()
            {
                return Ok(pid);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?
}
fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{
                GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            },
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code = 0;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == 259
        }
    }
}
async fn gone(pid: u32) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), async {
        while alive(pid) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    Ok(())
}
async fn fixture(lab: &Lab) -> Result<std::path::PathBuf> {
    let source = lab.root.path().join("failure-child.rs");
    std::fs::write(
        &source,
        r#"
use std::{io::Write, time::{Duration, Instant}};
fn main() {
    let a:Vec<String> = std::env::args().collect();
    let dir = std::path::Path::new(&a[2]);
    if a[1] == "detached" {
        #[cfg(unix)] {
            use std::os::unix::process::CommandExt;
            let mut command = std::process::Command::new(&a[0]);
            command.args(["wait", &a[2]]);
            unsafe { command.pre_exec(|| {
                unsafe extern "C" { fn setsid() -> i32; }
                if setsid() < 0 { return Err(std::io::Error::last_os_error()); }
                Ok(())
            }); }
            let mut child = command.spawn().unwrap();
            let until = Instant::now() + Duration::from_secs(5);
            while !dir.join("pid").exists() {
                assert!(child.try_wait().unwrap().is_none());
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    } else {
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("runs")).unwrap()
            .write_all(b"run\n").unwrap();
        std::fs::write(dir.join("pid"), std::process::id().to_string()).unwrap();
        if a[1] == "wait" {
            let until = Instant::now() + Duration::from_secs(30);
            while !dir.join("release").exists() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    println!("child output");
}
"#,
    )?;
    let program = lab.root.path().join(if cfg!(windows) {
        "failure-child.exe"
    } else {
        "failure-child"
    });
    let out = tokio::process::Command::new("rustc")
        .arg(source)
        .arg("-o")
        .arg(&program)
        .output()
        .await?;
    anyhow::ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(program)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_rejects_invalid_execution_without_cli_checks_or_accepting_a_job() -> Result<()> {
    let lab = Lab::new().await?;
    let base = execution(&lab, &binary(), &["--version"])?;
    let mut cases = vec![];
    for id in [String::new(), "x".repeat(129)] {
        let mut r = base.clone();
        r.request_id = id;
        cases.push((r, vec![], "INVALID_REQUEST"));
    }
    for cwd in ["relative".to_string(), format!("{}\0", base.cwd)] {
        let mut r = base.clone();
        r.cwd = cwd;
        cases.push((r, vec![], "INVALID_REQUEST"));
    }
    for (key, value) in [("", "v"), ("A=B", "v"), ("A\0", "v"), ("A", "v\0")] {
        let mut r = base.clone();
        r.env.insert(key.into(), value.into());
        cases.push((r, vec![], "INVALID_REQUEST"));
    }
    let mut r = base.clone();
    r.program.push('\0');
    cases.push((r, vec![], "INVALID_REQUEST"));
    let mut r = base.clone();
    r.args.push("bad\0arg".into());
    cases.push((r, vec![], "INVALID_REQUEST"));
    let mut r = base.clone();
    r.db_id = "another-database".into();
    cases.push((r, vec![], "DB_RESET"));
    #[cfg(windows)]
    {
        let mut r = base.clone();
        r.env.insert("Path".into(), "one".into());
        r.env.insert("PATH".into(), "two".into());
        cases.push((r, vec![], "INVALID_REQUEST"));
    }
    for (shell, input, code) in [
        ("unsupported", b"".as_slice(), "SHELL_UNSUPPORTED"),
        ("sh", b"\xff".as_slice(), "INVALID_SCRIPT"),
        ("cmd", "echo 中文".as_bytes(), "INVALID_SCRIPT"),
    ] {
        let mut r = base.clone();
        r.shell = Some(shell.into());
        r.input_size = input.len() as u64;
        r.input_sha256 = sha256(input);
        cases.push((r, input.to_vec(), code));
    }
    for arg in ["quote\"", "line\n", "line\r"] {
        let mut r = base.clone();
        r.shell = Some("cmd".into());
        r.args = vec![arg.into()];
        cases.push((r, vec![], "INVALID_SCRIPT_ARGUMENT"));
    }
    for (r, input, code) in cases {
        rejected(submit(&lab, &r, &input).await?, code);
        assert!(
            store(&lab)?.all()?.is_empty(),
            "rejected input created a task"
        );
    }
    let job = accepted(submit(&lab, &base, &[]).await?);
    assert_eq!(terminal(&lab, &job.job_id).await?.exit_code, Some(0));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn daemon_deduplicates_before_capacity_and_refuses_changed_execution_fields() -> Result<()> {
    let lab = Lab::new().await?;
    let program = fixture(&lab).await?;
    let dir = lab.root.path().join("dedup");
    std::fs::create_dir(&dir)?;
    let config_path = lab.target.join(".xrun/daemon.toml");
    let mut cfg: xrun::testing::config::DaemonConfig = xrun::testing::config::read(&config_path)?;
    cfg.max_concurrent_jobs = 1;
    xrun::testing::config::write(&config_path, &cfg)?;
    let base = execution(&lab, &program, &["wait", dir.to_str().unwrap()])?;
    let job = accepted(submit(&lab, &base, &[]).await?);
    pid_file(&dir.join("pid")).await?;
    assert_eq!(accepted(submit(&lab, &base, &[]).await?).job_id, job.job_id);
    let mut busy = base.clone();
    busy.request_id = "busy-retry".into();
    rejected(submit(&lab, &busy, &[]).await?, "DEVICE_BUSY");
    assert!(
        store(&lab)?
            .by_request(&lab.source_identity.device_id, &busy.request_id)?
            .is_none()
    );
    for field in ["program", "args", "cwd", "env", "timeout", "shell", "input"] {
        let mut changed = base.clone();
        let mut input = vec![];
        match field {
            "program" => changed.program = "another-program".into(),
            "args" => changed.args.push("extra".into()),
            "cwd" => changed.cwd = lab.source.to_string_lossy().into(),
            "env" => {
                changed.env.insert("DIFFERENT".into(), "value".into());
            }
            "timeout" => changed.timeout += 1,
            "shell" => changed.shell = Some("sh".into()),
            "input" => {
                input = b"different input".to_vec();
                changed.input_size = input.len() as u64;
                changed.input_sha256 = sha256(&input);
            }
            _ => unreachable!(),
        }
        rejected(submit(&lab, &changed, &input).await?, "REQUEST_CONFLICT");
    }
    std::fs::write(dir.join("release"), b"")?;
    assert_eq!(terminal(&lab, &job.job_id).await?.exit_code, Some(0));
    assert_eq!(accepted(submit(&lab, &base, &[]).await?).job_id, job.job_id);
    assert_eq!(std::fs::read(dir.join("runs"))?, b"run\n");
    let retried = accepted(submit(&lab, &busy, &[]).await?);
    assert_ne!(retried.job_id, job.job_id);
    assert_eq!(terminal(&lab, &retried.job_id).await?.exit_code, Some(0));
    assert_eq!(std::fs::read(dir.join("runs"))?, b"run\nrun\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_process_recording_kills_the_spawned_process_and_releases_capacity() -> Result<()> {
    let mut lab = Lab::new().await?;
    let program = fixture(&lab).await?;
    let db = database(&lab)?;
    for phase in ["starting", "running"] {
        let dir = lab.root.path().join(phase);
        std::fs::create_dir(&dir)?;
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_process BEFORE UPDATE OF data ON jobs
             WHEN NEW.state='{phase}' AND json_extract(NEW.data,'$.process.pid') IS NOT NULL
             BEGIN SELECT RAISE(FAIL,
                 'test process record failure pid=' || json_extract(NEW.data,'$.process.pid'));
             END;"
        ))?;
        let request = execution(&lab, &program, &["wait", dir.to_str().unwrap()])?;
        let job = accepted(submit(&lab, &request, &[]).await?);
        let failed = terminal(&lab, &job.job_id).await?;
        assert_eq!(failed.state, JobState::Failed);
        // The failed lifecycle transaction rolls back every write, including
        // trigger writes. Carry the PID in the injected error to prove cleanup.
        let failure = failed.error.context("missing process recording failure")?;
        let pid: u32 = failure
            .split_once("test process record failure pid=")
            .context("missing spawned PID in recording failure")?
            .1
            .split(|character: char| !character.is_ascii_digit())
            .next()
            .context("empty spawned PID")?
            .parse()?;
        gone(pid).await?;
        assert_eq!(store(&lab)?.active_count()?, 0);
        assert!(lab.daemon.try_wait()?.is_none());
        db.execute_batch("DROP TRIGGER fail_process;")?;
    }
    let job = accepted(submit(&lab, &execution(&lab, &binary(), &["--version"])?, &[]).await?);
    assert_eq!(terminal(&lab, &job.job_id).await?.exit_code, Some(0));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unsavable_result_stops_daemon_cleans_other_jobs_and_recovers_as_lost() -> Result<()> {
    let mut lab = Lab::new().await?;
    let program = fixture(&lab).await?;
    let db = database(&lab)?;
    let mut jobs = vec![];
    let mut pids = vec![];
    for name in ["finishing", "still-running"] {
        let dir = lab.root.path().join(name);
        std::fs::create_dir(&dir)?;
        let request = execution(&lab, &program, &["wait", dir.to_str().unwrap()])?;
        jobs.push(accepted(submit(&lab, &request, &[]).await?));
        pids.push(pid_file(&dir.join("pid")).await?);
    }
    db.execute_batch(
        "CREATE TRIGGER fail_result BEFORE UPDATE OF data ON jobs
         WHEN NEW.state NOT IN ('starting','running')
         BEGIN SELECT RAISE(FAIL, 'test result write failure'); END;",
    )?;
    std::fs::write(lab.root.path().join("finishing/release"), b"")?;
    let status = tokio::time::timeout(Duration::from_secs(15), lab.daemon.wait()).await??;
    assert!(
        !status.success(),
        "unsaved results must stop the daemon with an error"
    );
    for pid in pids {
        gone(pid).await?;
    }
    for job in &jobs {
        assert!(!store(&lab)?.get(&job.job_id)?.unwrap().state.terminal());
    }
    let wait = cli(&lab.source, &["target1", "wait", &jobs[0].job_id, "--json"]).await;
    assert_eq!(wait.status.code(), Some(75));
    db.execute_batch("DROP TRIGGER fail_result;")?;
    lab.daemon = logged(&lab.target, &["daemon"], "recovered-target")?.spawn()?;
    online(&lab.source, "target1").await?;
    for job in &jobs {
        let recovered = store(&lab)?.get(&job.job_id)?.unwrap();
        assert_eq!(recovered.state, JobState::Lost);
        assert!(recovered.error.unwrap().contains("RESULT_LOST"));
    }
    for name in ["finishing", "still-running"] {
        assert_eq!(
            std::fs::read(lab.root.path().join(name).join("runs"))?,
            b"run\n"
        );
    }
    assert_eq!(store(&lab)?.active_count()?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn log_quota_and_write_failure_preserve_exit_status_and_report_incomplete_output()
-> Result<()> {
    let mut lab = Lab::new().await?;
    let program = fixture(&lab).await?;
    let db = database(&lab)?;
    for (quota, reason, code) in [
        (false, "test capture write failure", "LOG_INCOMPLETE"),
        (true, "TRUNCATED", "LOG_TRUNCATED"),
    ] {
        let dir = lab.root.path().join(format!("output-{quota}"));
        std::fs::create_dir(&dir)?;
        let request = execution(&lab, &program, &["wait", dir.to_str().unwrap()])?;
        let job = accepted(submit(&lab, &request, &[]).await?);
        pid_file(&dir.join("pid")).await?;
        if quota {
            store(&lab)?.append(&job.job_id, "stdout", b"retained")?;
            db.execute(
                "UPDATE log_sizes SET bytes=?2 WHERE job=?1",
                rusqlite::params![job.job_id, MAX_FILE as i64],
            )?;
            db.execute(
                "UPDATE meta SET value=(SELECT SUM(bytes) FROM log_sizes) WHERE key='log_bytes'",
                [],
            )?;
        } else {
            db.execute_batch("CREATE TRIGGER fail_logs BEFORE INSERT ON logs BEGIN SELECT RAISE(ABORT, 'test capture write failure'); END;")?;
        }
        std::fs::write(dir.join("release"), b"")?;
        let finished = terminal(&lab, &job.job_id).await?;
        assert_eq!(finished.state, JobState::Exited);
        assert_eq!(finished.exit_code, Some(0));
        assert!(!finished.output_complete);
        assert!(finished.incomplete_reason.unwrap().contains(reason));
        let logs = store(&lab)?.logs(&job.job_id, 0)?;
        assert_eq!(logs.len(), usize::from(quota));
        if quota {
            assert_eq!(logs[0].data_base64, "cmV0YWluZWQ=");
        }
        let output = cli(&lab.source, &["target1", "logs", &job.job_id, "--json"]).await;
        assert_eq!(output.status.code(), Some(1));
        let error: serde_json::Value = serde_json::from_slice(&output.stderr)?;
        assert_eq!(error["code"], code);
        assert!(lab.daemon.try_wait()?.is_none());
        if !quota {
            db.execute_batch("DROP TRIGGER fail_logs;")?;
        }
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn escaped_process_holding_output_does_not_keep_the_job_running() -> Result<()> {
    let lab = Lab::new().await?;
    let program = fixture(&lab).await?;
    let dir = lab.root.path().join("detached");
    std::fs::create_dir(&dir)?;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("release"), b"");
        }
    }
    let _cleanup = Cleanup(dir.clone());
    let request = execution(&lab, &program, &["detached", dir.to_str().unwrap()])?;
    let job = accepted(submit(&lab, &request, &[]).await?);
    let pid = pid_file(&dir.join("pid")).await?;
    let finished = terminal(&lab, &job.job_id).await?;
    assert!(
        alive(pid),
        "fixture did not retain its inherited output pipe"
    );
    assert_eq!(finished.state, JobState::Exited);
    assert_eq!(finished.exit_code, Some(0));
    assert!(!finished.output_complete);
    assert_eq!(
        finished.incomplete_reason.as_deref(),
        Some("DETACHED_OUTPUT")
    );
    assert_eq!(store(&lab)?.active_count()?, 0);
    let output = cli(&lab.source, &["target1", "logs", &job.job_id, "--json"]).await;
    assert_eq!(output.status.code(), Some(1));
    let error: serde_json::Value = serde_json::from_slice(&output.stderr)?;
    assert_eq!(error["code"], "LOG_INCOMPLETE");
    std::fs::write(dir.join("release"), b"")?;
    gone(pid).await?;
    Ok(())
}
