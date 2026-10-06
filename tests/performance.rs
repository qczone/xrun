//! Optional measurements using actual CLI processes and isolated task databases.
mod common;
use anyhow::{Context, Result, ensure};
use common::*;
use serde::Serialize;
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    time::Instant,
};

#[derive(Serialize)]
struct Usage {
    cpu_seconds: f64,
    wakeups: Option<u64>,
}

#[cfg(target_os = "macos")]
fn usage(pid: u32) -> Result<Usage> {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let result = unsafe {
        libc::proc_pid_rusage(
            pid as i32,
            libc::RUSAGE_INFO_V2,
            std::ptr::from_mut(&mut info).cast(),
        )
    };
    ensure!(result == 0, "could not read process resource usage");
    Ok(Usage {
        cpu_seconds: (info.ri_user_time + info.ri_system_time) as f64 / 1_000_000_000.0,
        wakeups: Some(info.ri_interrupt_wkups),
    })
}

#[cfg(target_os = "linux")]
fn usage(pid: u32) -> Result<Usage> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .context("process stat")?
        .1
        .split_whitespace()
        .collect();
    let ticks = fields[11].parse::<u64>()? + fields[12].parse::<u64>()?;
    let frequency = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    ensure!(frequency > 0, "invalid process CPU frequency");
    Ok(Usage {
        cpu_seconds: ticks as f64 / frequency as f64,
        wakeups: None,
    })
}

#[cfg(windows)]
fn usage(pid: u32) -> Result<Usage> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, FILETIME},
        System::Threading::{GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        ensure!(!handle.is_null(), "could not open process usage handle");
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let result = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(handle);
        ensure!(result != 0, "could not read process CPU usage");
        let ticks =
            |time: FILETIME| (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
        Ok(Usage {
            cpu_seconds: (ticks(kernel) + ticks(user)) as f64 / 10_000_000.0,
            wakeups: None,
        })
    }
}

async fn idle(pid: u32, seconds: u64) -> Result<serde_json::Value> {
    // Exclude handshake and setup work from the steady-state sample.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let before = usage(pid)?;
    let started = Instant::now();
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    let after = usage(pid)?;
    let elapsed = started.elapsed().as_secs_f64();
    Ok(serde_json::json!({
        "sample_seconds": elapsed,
        "cpu_seconds_per_minute": (after.cpu_seconds - before.cpu_seconds) * 60.0 / elapsed,
        "wakeups_per_minute": before.wakeups.zip(after.wakeups)
            .map(|(before, after)| (after - before) as f64 * 60.0 / elapsed),
    }))
}

async fn idle_sessions(lab: &Lab, seconds: u64) -> Result<serde_json::Value> {
    let pid = lab.daemon.id().context("target daemon PID")?;
    let empty = idle(pid, seconds).await?;
    let backend = TcpListener::bind("127.0.0.1:0").await?;
    let port = backend.local_addr()?.port();
    let mut forwarding = logged(
        &lab.source,
        &["target1", "forward", &format!("0:{port}"), "--json"],
        "benchmark-forward",
    )?
    .stdout(Stdio::piped())
    .spawn()?;
    let mut reader = BufReader::new(forwarding.stdout.take().context("forward stdout")?);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line)).await??;
    let address: serde_json::Value = serde_json::from_str(&line)?;
    let address = address["local_address"]
        .as_str()
        .context("forward address")?;
    let mut sockets = Vec::new();
    for _ in 0..10 {
        let mut client = TcpStream::connect(address).await?;
        let (mut server, _) =
            tokio::time::timeout(Duration::from_secs(10), backend.accept()).await??;
        client.write_all(b"ready").await?;
        let mut bytes = [0; 5];
        server.read_exact(&mut bytes).await?;
        server.write_all(&bytes).await?;
        client.read_exact(&mut bytes).await?;
        sockets.push((client, server));
    }
    let ten_sessions = idle(pid, seconds).await?;
    drop(sockets);
    forwarding.start_kill()?;
    forwarding.wait().await?;

    Ok(serde_json::json!({"zero_sessions":empty,"ten_sessions":ten_sessions}))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run with desktop/scripts/benchmark-core.ts; timings are measurements, not CI thresholds"]
async fn measure_idle_sessions_and_dense_logs() -> Result<()> {
    let mut lab = Lab::new().await?;
    let seconds: u64 = std::env::var("XRUN_BENCH_IDLE_SECONDS")
        .unwrap_or_else(|_| "20".into())
        .parse()?;
    ensure!(
        (5..=120).contains(&seconds),
        "idle sample must be 5..120 seconds"
    );
    let mode = std::env::var("XRUN_BENCH_LOG_MODE").unwrap_or_else(|_| "dense".into());
    ensure!(
        matches!(mode.as_str(), "dense" | "bursts"),
        "unknown log measurement mode"
    );
    let measured_idle = if mode == "dense" {
        Some(idle_sessions(&lab, seconds).await?)
    } else {
        None
    };

    let source = lab.root.path().join("producer.rs");
    let producer = lab.root.path().join(if cfg!(windows) {
        "producer.exe"
    } else {
        "producer"
    });
    std::fs::write(&source, include_str!("fixtures/log-producer.rs"))?;
    let compiled = tokio::process::Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&producer)
        .output()
        .await?;
    ensure!(compiled.status.success(), "log fixture compilation failed");
    let mut runs = Vec::new();
    for _ in 0..3 {
        let started = Instant::now();
        let job = json(
            cli(
                &lab.source,
                &[
                    "target1",
                    "start",
                    "--json",
                    "--",
                    &producer.to_string_lossy(),
                    &mode,
                ],
            )
            .await,
        );
        let id = job["job_id"].as_str().context("job ID")?;
        let mut waiting = command(&lab.source, &["target1", "wait", id, "--json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut queries = Vec::new();
        while waiting.try_wait()?.is_none() {
            let query_started = Instant::now();
            json(cli(&lab.source, &["target1", "jobs", "--json"]).await);
            queries.push(query_started.elapsed().as_secs_f64() * 1000.0);
        }
        let completed = json(waiting.wait_with_output().await?);
        ensure!(completed["job"]["exit_code"] == 0, "producer failed");
        let elapsed = started.elapsed().as_secs_f64();
        let database = rusqlite::Connection::open_with_flags(
            lab.target.join(".xrun/daemon.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        let (chunks, bytes): (i64, i64) = database.query_row(
            "SELECT COUNT(*),COALESCE(SUM(length(bytes)),0) FROM logs WHERE job=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        ensure!(
            bytes == 5_000_000,
            "benchmark output was not fully persisted"
        );
        runs.push(serde_json::json!({"elapsed_seconds":elapsed,"persisted_chunks":chunks,"jobs_latency_ms":queries}));
    }
    let result = serde_json::json!({
        "platform": std::env::consts::OS,
        "architecture": std::env::consts::ARCH,
        "idle": measured_idle,
        "logs": {"mode":mode,"lines":100_000,"line_bytes":50,"runs":runs},
        "not_measured": ["file-read syscall count", "fsync syscall count"],
    });
    if let Ok(path) = std::env::var("XRUN_BENCH_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&result)?)?;
    }
    println!("{result}");
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    Ok(())
}
