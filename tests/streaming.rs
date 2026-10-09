mod common;
use anyhow::{Context, Result};
use common::*;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
};

async fn fixture(lab: &Lab) -> Result<String> {
    let source = lab.root.path().join("stream-child.rs");
    std::fs::write(
        &source,
        r#"
use std::{io::{Read, Write}, time::Duration};
fn main() {
    let a:Vec<String> = std::env::args().collect();
    match a[1].as_str() {
        "copy" => { let mut b=vec![]; std::io::stdin().read_to_end(&mut b).unwrap(); std::io::stdout().write_all(&b).unwrap(); std::io::stderr().write_all(b"stderr after EOF").unwrap(); },
        "stream" => { let mut b=[0;65536]; loop { let n=std::io::stdin().read(&mut b).unwrap(); if n==0 {break} std::io::stdout().write_all(&b[..n]).unwrap(); } },
        "exit" => { print!("early exit"); std::io::stdout().flush().unwrap(); std::process::exit(7); },
        "sleep" => { std::fs::write(&a[2], std::process::id().to_string()).unwrap(); std::thread::sleep(Duration::from_secs(60)); },
        "tree" => { let _child=std::process::Command::new(&a[0]).args(["sleep", &a[2]]).spawn().unwrap(); std::thread::sleep(Duration::from_secs(60)); },
        "env" => print!("{}", std::env::var("XRUN_STREAM_ENV").unwrap()),
        _=>panic!(),
    }
}"#,
    )?;
    let executable = lab.root.path().join(if cfg!(windows) {
        "stream-child.exe"
    } else {
        "stream-child"
    });
    let output = Command::new("rustc")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(executable.to_string_lossy().into())
}

async fn input(home: &Path, args: &[&str], bytes: &[u8]) -> Result<std::process::Output> {
    let mut child = command(home, args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let send = async {
        stdin.write_all(bytes).await?;
        drop(stdin);
        Ok::<_, anyhow::Error>(())
    };
    let receive = async { Ok::<_, anyhow::Error>(child.wait_with_output().await?) };
    let ((), output) = tokio::try_join!(send, receive)?;
    Ok(output)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_preserves_binary_eof_stderr_exit_and_records_jobs_without_environment_values()
-> Result<()> {
    tokio::time::timeout(Duration::from_secs(40), async {
        let lab = Lab::new().await?;
        let child = fixture(&lab).await?;
        for options in [vec!["start", "-i"], vec!["-i", "--json"], vec!["-i", "--stdin"], vec!["-i", "--request-id", "invalid"], vec!["-i", "--script", "sh"]] {
            let mut args=vec!["target1"]; args.extend(options); args.extend(["--", &child, "exit"]);
            assert_eq!(cli(&lab.source,&args).await.status.code(),Some(2));
        }
        let content:Vec<u8> = (0..3_000_000).map(|i| (i%256) as u8).collect();
        for operation in ["copy", "stream"] {
            let output = input(&lab.source, &["target1", "-i", "--", &child, operation], &content).await?;
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
            assert_eq!(output.stdout.len(), content.len());
            assert_eq!(xrun::protocol::sha256(&output.stdout),xrun::protocol::sha256(&content));
            if operation == "copy" { assert_eq!(output.stderr, b"stderr after EOF"); }
        }
        // Keep stdin open: a process that never reads it must still return its
        // output and exit code promptly.
        let mut early = command(&lab.source,&["target1","-i","--",&child,"exit"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let stdin = early.stdin.take().unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5),early.wait_with_output()).await??;
        assert_eq!(result.status.code(),Some(7));
        assert_eq!(result.stdout,b"early exit"); drop(stdin);
        let output=input(&lab.source,&["target1","-i","--env","XRUN_STREAM_ENV=explicit","--",&child,"env"], b"").await?;
        assert_eq!(ok(output),"explicit");
        let jobs = json(cli(&lab.source,&["target1","jobs","--json"]).await);
        assert!(jobs.as_array().unwrap().iter().all(|job| job["kind"] == "stream_exec"));
        assert!(!jobs.as_array().unwrap().is_empty());
        let db=rusqlite::Connection::open(lab.target.join(".xrun/daemon.db"))?;
        let record:String=db.query_row("SELECT params_json || result_json FROM jobs WHERE kind='stream_exec' AND json_extract(result_json,'$.stdout_bytes')=3000000 LIMIT 1",[],|r|r.get(0))?;
        assert!(!record.contains("explicit"));
        Ok::<_, anyhow::Error>(())
    }).await??;
    Ok(())
}

async fn recorded_pid(path: &Path, child: &mut Child) -> Result<u32> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(path)
                && let Ok(pid) = pid.parse()
            {
                return Ok(pid);
            }
            if let Some(status) = child.try_wait()? {
                let mut error = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    stderr.read_to_string(&mut error).await?;
                }
                anyhow::bail!(
                    "stream CLI exited before creating {}: {status}: {error}",
                    path.display()
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .with_context(|| format!("process did not create {}", path.display()))?
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
            let success = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            success != 0 && code == 259
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn streaming_timeout_disconnect_pause_and_capacity_clean_process_trees() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(50), async {
        let mut lab = Lab::new().await?;
        let child = fixture(&lab).await?;
        for action in ["timeout", "disconnect", "pause"] {
            let pid_path = lab.root.path().join(format!("{action}.pid"));
            let pid_arg = pid_path.to_string_lossy();
            let args = vec![
                "target1",
                "-i",
                "--timeout",
                // The execution deadline includes executable startup. Leave
                // enough time for a cold launch to create the descendant;
                // otherwise exit 124 is valid before the PID file exists.
                if action == "timeout" { "5" } else { "0" },
                "--",
                &child,
                "tree",
                &pid_arg,
            ];
            let mut cli_child = command(&lab.source, &args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()?;
            let pid = recorded_pid(&pid_path, &mut cli_child).await?;
            assert!(alive(pid));
            if action == "disconnect" {
                cli_child.start_kill()?;
            }
            if action == "pause" {
                ok(cli(&lab.target, &["daemon", "pause"]).await);
            }
            let status = tokio::time::timeout(Duration::from_secs(12), cli_child.wait())
                .await
                .with_context(|| format!("{action}: CLI did not exit"))??;
            if action == "timeout" {
                assert_eq!(status.code(), Some(124));
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                while alive(pid) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .with_context(|| format!("{action}: descendant {pid} survived"))?;
            if action == "pause" {
                ok(cli(&lab.target, &["daemon", "resume"]).await);
            }
        }
        // Reliable and connection-bound processes share the job limit.
        let mut raw = vec![];
        for n in 0..4 {
            let pid_path = lab.root.path().join(format!("capacity-{n}.pid"));
            let mut proc = command(
                &lab.source,
                &[
                    "target1",
                    "-i",
                    "--timeout",
                    "0",
                    "--",
                    &child,
                    "sleep",
                    &pid_path.to_string_lossy(),
                ],
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
            let pid = recorded_pid(&pid_path, &mut proc).await?;
            raw.push((proc, pid));
        }
        let refused = cli(&lab.source, &["target1", "start", "--", &child, "exit"]).await;
        assert_eq!(refused.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&refused.stderr).contains("DEVICE_BUSY"));
        for (mut proc, pid) in raw {
            proc.start_kill()?;
            proc.wait().await?;
            tokio::time::timeout(Duration::from_secs(5), async {
                while alive(pid) {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .with_context(|| format!("capacity: process {pid} survived"))?;
        }
        let pid_path = lab.root.path().join("daemon-stop.pid");
        let mut proc = command(
            &lab.source,
            &[
                "target1",
                "-i",
                "--timeout",
                "0",
                "--",
                &child,
                "tree",
                &pid_path.to_string_lossy(),
            ],
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
        let pid = recorded_pid(&pid_path, &mut proc).await?;
        ok(cli(&lab.target, &["daemon", "stop"]).await);
        assert!(
            tokio::time::timeout(Duration::from_secs(12), lab.daemon.wait())
                .await
                .context("daemon did not stop")??
                .success()
        );
        let _ = tokio::time::timeout(Duration::from_secs(5), proc.wait())
            .await
            .context("daemon stop: CLI did not exit")??;
        tokio::time::timeout(Duration::from_secs(5), async {
            while alive(pid) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .with_context(|| format!("daemon stop: descendant {pid} survived"))?;
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}
