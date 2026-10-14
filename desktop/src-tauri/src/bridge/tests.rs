use super::*;
use std::io::{BufReader, PipeReader, PipeWriter};

struct Peer {
    requests: BufReader<PipeReader>,
    responses: PipeWriter,
}

impl Peer {
    fn start() -> Result<(Arc<Connection>, Self)> {
        let (requests, input) = std::io::pipe()?;
        let (output, mut responses) = std::io::pipe()?;
        writeln!(
            responses,
            "{}",
            json!({"ready": {"version": env!("CARGO_PKG_VERSION")}})
        )?;
        let connection =
            Connection::start(Box::new(input), Box::new(BufReader::new(output)), None)?;
        Ok((
            connection,
            Self {
                requests: BufReader::new(requests),
                responses,
            },
        ))
    }

    fn next(&mut self) -> Result<Option<Value>> {
        let mut line = String::new();
        if self.requests.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(serde_json::from_str(&line)?))
    }

    fn respond(&mut self, id: &Value, result: Value) -> Result<()> {
        writeln!(self.responses, "{}", json!({"id": id, "result": result}))?;
        Ok(())
    }
}

fn code(error: anyhow::Error) -> String {
    CommandError::from_error(error).code
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_fails_other_waiters_and_retry_reconnects_without_replaying_a_change() -> Result<()>
{
    let (stalled, peer) = Peer::start()?;
    let mut other = stalled.begin("settings", json!({}), None)?;
    let error = stalled
        .request(
            "pause_access",
            json!({"paused": true}),
            None,
            WaitPolicy::Deadline(Duration::from_millis(30)),
        )
        .await
        .unwrap_err();
    assert_eq!(code(error), "HELPER_FAILED");
    assert!(stalled.closed.load(Ordering::Acquire));
    assert_eq!(code(other.receive().await.unwrap_err()), "HELPER_FAILED");
    assert!(stalled.pending.lock().unwrap().is_empty());
    drop(peer);

    let (fresh, mut peer) = Peer::start()?;
    let server = std::thread::spawn(move || -> Result<()> {
        let request = peer.next()?.unwrap();
        // Recovery only connects. The timed-out mutation is never resubmitted.
        assert_eq!(request["method"], "local_status");
        peer.respond(&request["id"], json!({"joined": false}))
    });
    let mut cache = Some(stalled.clone());
    let restored = cached_connection(&mut cache, || Ok(fresh))?;
    assert!(!Arc::ptr_eq(&restored, &stalled));
    let result = restored
        .request(
            "local_status",
            json!({}),
            None,
            WaitPolicy::Deadline(Duration::from_secs(2)),
        )
        .await?;
    assert_eq!(result, json!({"joined": false}));
    restored.close();
    server.join().unwrap()?;
    Ok(())
}

#[test]
fn blocking_timeout_invalidates_a_helper_even_when_its_input_pipe_is_full() -> Result<()> {
    let (connection, peer) = Peer::start()?;
    // The peer deliberately never reads: serializing/enqueuing a request must
    // not wait for a pipe write before starting its response deadline.
    let params = json!({"payload": "x".repeat(1024 * 1024)});
    let error =
        get_on_connection::<Value>(&connection, "settings", params, Duration::from_millis(30))
            .unwrap_err();
    assert_eq!(code(error), "HELPER_FAILED");
    assert!(connection.closed.load(Ordering::Acquire));
    assert!(connection.pending.lock().unwrap().is_empty());
    drop(peer);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_timeout_also_covers_a_blocked_pipe_writer() -> Result<()> {
    let (connection, peer) = Peer::start()?;
    let operation = connection.request(
        "save_settings",
        json!({"payload": "x".repeat(1024 * 1024)}),
        None,
        WaitPolicy::Deadline(Duration::from_millis(30)),
    );
    let error = tokio::time::timeout(Duration::from_secs(3), operation)
        .await?
        .unwrap_err();
    assert_eq!(code(error), "HELPER_FAILED");
    assert!(connection.closed.load(Ordering::Acquire));
    drop(peer);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timeout_kills_and_reaps_a_real_helper_with_a_full_input_pipe() -> Result<()> {
    struct Cleanup(Arc<Connection>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            self.0.close();
        }
    }
    let directory = tempfile::tempdir()?;
    let source = directory.path().join("helper.rs");
    let executable = directory.path().join(if cfg!(windows) {
        "helper.exe"
    } else {
        "helper"
    });
    std::fs::write(
        &source,
        r#"
fn main() {
    println!("{{\"ready\":{{\"version\":\"{}\"}}}}", env!("XRUN_FIXTURE_VERSION"));
    loop { std::thread::park(); }
}
"#,
    )?;
    let compiled = std::process::Command::new("rustc")
        .args(["--edition=2024", "-o"])
        .arg(&executable)
        .arg(source)
        .env("XRUN_FIXTURE_VERSION", env!("CARGO_PKG_VERSION"))
        .output()?;
    anyhow::ensure!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let mut command = std::process::Command::new(executable);
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    let input = child.stdin.take().unwrap();
    let output = child.stdout.take().unwrap();
    let connection = Connection::start(
        Box::new(input),
        Box::new(BufReader::new(output)),
        Some(child),
    )?;
    let _cleanup = Cleanup(connection.clone());
    let operation = connection.request(
        "save_settings",
        json!({"payload": "x".repeat(1024 * 1024)}),
        None,
        WaitPolicy::Deadline(Duration::from_millis(30)),
    );
    let error = tokio::time::timeout(Duration::from_secs(3), operation)
        .await?
        .unwrap_err();
    assert_eq!(code(error), "HELPER_FAILED");
    assert!(connection.closed.load(Ordering::Acquire));
    assert!(connection.child.lock().unwrap().is_none());
    assert!(connection.pending.lock().unwrap().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_operations_continue_while_health_probes_are_answered() -> Result<()> {
    for method in [
        "cloudflare_accounts",
        "create_cloudflare_network",
        "logout_cloudflare",
        "cancel_cloudflare_authorization",
        "remote_file_upload",
        "remote_file_download",
        "remote_file_resume",
        "remote_screenshot",
        "save_activity_attachment",
    ] {
        assert!(matches!(wait_policy(method), WaitPolicy::Responsive { .. }));
    }
    let (connection, mut peer) = Peer::start()?;
    let server = std::thread::spawn(move || -> Result<usize> {
        let operation = peer.next()?.unwrap();
        let mut probes = 0;
        while let Some(request) = peer.next()? {
            assert_eq!(request["method"], "ping");
            probes += 1;
            peer.respond(&request["id"], Value::Null)?;
            if probes == 3 {
                peer.respond(&operation["id"], json!({"completed": true}))?;
                return Ok(probes);
            }
        }
        anyhow::bail!("operation ended before its health probes");
    });
    let operation = connection.request(
        "cloudflare_accounts",
        json!({}),
        None,
        WaitPolicy::Responsive {
            interval: Duration::from_millis(10),
            timeout: Duration::from_secs(1),
        },
    );
    let result = tokio::time::timeout(Duration::from_secs(5), operation).await??;
    assert_eq!(result, json!({"completed": true}));
    assert!(connection.pending.lock().unwrap().is_empty());
    connection.close();
    assert_eq!(server.join().unwrap()?, 3);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long_operations_fail_when_the_helper_stops_answering_probes() -> Result<()> {
    let (connection, peer) = Peer::start()?;
    let operation = connection.request(
        "remote_file_download",
        json!({}),
        None,
        WaitPolicy::Responsive {
            interval: Duration::from_millis(10),
            timeout: Duration::from_millis(30),
        },
    );
    let error = tokio::time::timeout(Duration::from_secs(3), operation)
        .await?
        .unwrap_err();
    assert_eq!(code(error), "HELPER_FAILED");
    assert!(connection.closed.load(Ordering::Acquire));
    assert!(connection.pending.lock().unwrap().is_empty());
    drop(peer);
    Ok(())
}
