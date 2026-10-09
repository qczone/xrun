mod common;
use anyhow::{Context, Result};
use common::*;
use std::path::Path;
use xrun::testing::{
    config::{self, DaemonConfig},
    protocol::{JobContext, VERSION},
};

fn manager_log(home: &Path) -> Result<String> {
    let base = std::env::var_os("XRUN_TEST_LOG_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| home.join("test-logs"));
    let hash = xrun::protocol::sha256(home.to_string_lossy().as_bytes());
    Ok(std::fs::read_to_string(
        base.join(format!("manager-{}.log", &hash[..12])),
    )?)
}
async fn version(home: &Path) {
    let binary = binary();
    assert_eq!(
        ok(cli(
            home,
            &["target1", "--", binary.to_str().unwrap(), "--version"]
        )
        .await)
        .trim(),
        format!("xrun {VERSION}")
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_reuses_completed_sessions_refreshes_defaults_and_discards_stale_connections()
-> Result<()> {
    let mut lab = Lab::new().await?;
    for _ in 0..4 {
        version(&lab.source).await;
    }
    let log = manager_log(&lab.source)?;
    assert_eq!(log.matches("opened operation session").count(), 1, "{log}");
    assert_eq!(log.matches("reusing operation session").count(), 3, "{log}");
    // A cached Ready cannot keep supplying an obsolete default directory.
    let cwd = lab.target.join("new-cwd");
    std::fs::create_dir(&cwd)?;
    let path = lab.target.join(".xrun/daemon.toml");
    let mut cfg: DaemonConfig = config::read(&path)?;
    cfg.default_cwd = Some(cwd.clone());
    config::write(&path, &cfg)?;
    let binary = binary();
    let job = json(
        cli(
            &lab.source,
            &[
                "target1",
                "start",
                "--json",
                "--",
                binary.to_str().unwrap(),
                "--version",
            ],
        )
        .await,
    );
    assert_eq!(job["params"]["cwd"].as_str(), cwd.to_str());
    // A denial on a previously cached connection must still prevent execution.
    cfg.deny_from.push(lab.source_identity.device_id.clone());
    config::write(&path, &cfg)?;
    let denied = cli(
        &lab.source,
        &["target1", "--", binary.to_str().unwrap(), "--version"],
    )
    .await;
    assert_eq!(denied.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&denied.stderr).contains("SOURCE_NOT_ALLOWED"));
    cfg.deny_from.clear();
    config::write(&path, &cfg)?;
    version(&lab.source).await;
    // The first request after a target reset must use the new database ID.
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    ok(cli(&lab.target, &["daemon", "reset"]).await);
    lab.daemon = logged(&lab.target, &["daemon"], "target")?.spawn()?;
    online(&lab.source, "target1").await?;
    version(&lab.source).await;
    // Execution remains available without a local daemon; stale IPC metadata
    // from a crash is also safely ignored before any operation is submitted.
    let endpoint = std::fs::read(lab.source.join(".xrun/daemon-ipc.json"))?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    config::atomic_private_write(&lab.source.join(".xrun/daemon-ipc.json"), &endpoint)?;
    version(&lab.source).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cli_requests_keep_outputs_and_request_ids_separate() -> Result<()> {
    let lab = Lab::new().await?;
    let path = lab.target.join(".xrun/daemon.toml");
    let mut cfg: DaemonConfig = config::read(&path)?;
    cfg.max_concurrent_jobs = 16;
    config::write(&path, &cfg)?;
    version(&lab.source).await;
    let mut tasks = tokio::task::JoinSet::new();
    for index in 0..8 {
        let home = lab.source.clone();
        tasks.spawn(async move {
            let request = format!("parallel-{index}");
            let binary = binary();
            let job = json(
                cli(
                    &home,
                    &[
                        "target1",
                        "start",
                        "--json",
                        "--request-id",
                        &request,
                        "--",
                        binary.to_str().unwrap(),
                        "--version",
                    ],
                )
                .await,
            );
            assert_eq!(job["request_id"], request);
            let job_id = job["job_id"].as_str().context("job id")?;
            let waited = json(cli(&home, &["target1", "wait", job_id, "--json"]).await);
            assert_eq!(waited["job"]["request_id"], request);
            assert_eq!(waited["job"]["result"]["exit_code"], 0);
            Ok::<_, anyhow::Error>(())
        });
    }
    while let Some(result) = tasks.join_next().await {
        result??;
    }
    Ok(())
}

async fn local_wire(home: &Path) -> Result<(xrun::testing::net::Ws, serde_json::Value)> {
    let endpoint: serde_json::Value =
        serde_json::from_slice(&std::fs::read(home.join(".xrun/daemon-ipc.json"))?)?;
    let address = endpoint["address"].as_str().context("IPC address")?;
    #[cfg(unix)]
    let io: xrun::testing::net::Io = Box::new(tokio::net::UnixStream::connect(address).await?);
    #[cfg(windows)]
    let io: xrun::testing::net::Io =
        Box::new(tokio::net::windows::named_pipe::ClientOptions::new().open(address)?);
    Ok((
        tokio_tungstenite::WebSocketStream::from_raw_socket(
            xrun::testing::net::SocketIo::new(io),
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await,
        endpoint,
    ))
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn private_ipc_rejects_bad_tokens_stale_stops_and_incomplete_releases() -> Result<()> {
    use xrun::testing::{
        net,
        protocol::{Data, sha256},
    };
    let lab = Lab::new().await?;
    version(&lab.source).await;
    let id = &lab.source_identity;
    let identity = sha256(format!("{}\0{}\0{}", id.device_id, id.ca_pem, id.cert_pem).as_bytes());
    let (mut ws, endpoint) = local_wire(&lab.source).await?;
    let mut opening = serde_json::json!({"local":"open","token":"wrong","version":VERSION,"identity":identity,"target":lab.target_identity.device_id});
    net::send(&mut ws, &opening).await?;
    assert!(
        matches!(net::receive::<Data>(&mut ws).await?, Data::Error{code,..} if code == "UNAUTHENTICATED")
    );
    drop(ws);
    let (mut ws, _) = local_wire(&lab.source).await?;
    net::send(
        &mut ws,
        &serde_json::json!({"local":"stop","token":endpoint["token"],"generation":"old"}),
    )
    .await?;
    assert!(
        matches!(net::receive::<Data>(&mut ws).await?, Data::Error{code,..} if code == "DAEMON_CHANGED")
    );
    drop(ws);
    let (mut ws, _) = local_wire(&lab.source).await?;
    opening["token"] = endpoint["token"].clone();
    net::send(&mut ws, &opening).await?;
    assert!(matches!(
        net::receive::<Data>(&mut ws).await?,
        Data::Ready { .. }
    ));
    net::send(&mut ws, &serde_json::json!({"local":"release"})).await?;
    assert!(
        matches!(net::receive::<Data>(&mut ws).await?, Data::Error{code,..} if code == "INVALID_MESSAGE")
    );
    drop(ws);
    version(&lab.source).await;
    let log = manager_log(&lab.source)?;
    assert_eq!(
        log.matches("opened operation session").count(),
        2,
        "abandoned session was returned to the cache: {log}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changed_upload_is_rejected_without_overwriting_the_destination() -> Result<()> {
    use xrun::testing::{
        net,
        protocol::{Data, Request},
    };
    let lab = Lab::new().await?;
    let source = lab.source.join("changing-file");
    let target = lab.target.join("protected-file");
    std::fs::write(&source, b"before")?;
    std::fs::write(&target, b"keep-existing")?;
    let (file, size, hash) = xrun::testing::transfer::prepare_upload(&source)?;
    std::fs::write(&source, b"after!")?;
    let mut ws = peer_session(
        &lab.source,
        &lab.source_identity,
        &lab.target_identity.device_id,
    )
    .await?;
    let Data::Ready { db_id, .. } = net::receive::<Data>(&mut ws).await? else {
        anyhow::bail!("ready expected")
    };
    net::send(
        &mut ws,
        &Data::Request {
            request: Request::Push {
                context: JobContext::new(&db_id),
                path: target.to_string_lossy().into(),
                cwd: None,
                size,
                sha256: hash,
                mkdir: false,
                no_overwrite: false,
                expect: None,
            },
        },
    )
    .await?;
    assert!(matches!(
        net::receive::<Data>(&mut ws).await?,
        Data::Accepted { fresh: true, .. }
    ));
    net::send_file(&mut ws, &file).await?;
    assert!(
        matches!(net::receive::<Data>(&mut ws).await?, Data::Error { code, .. } if code == "CHECKSUM_MISMATCH")
    );
    assert_eq!(std::fs::read(target)?, b"keep-existing");
    Ok(())
}
