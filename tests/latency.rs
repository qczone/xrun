//! Controlled loopback acceptance: config commit to closure, output trigger to visibility.
mod common;
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use std::{path::Path, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpListener, time::Instant};
use xrun::testing::{
    config::{self, DaemonConfig},
    crypto,
    membership::{Manager, RosterCache},
    net::{self, Ws},
    protocol::{Data, Request},
};

const ACCESS_BUDGET: Duration = Duration::from_millis(200);
const OUTPUT_BUDGET: Duration = Duration::from_millis(100);

async fn subscription(lab: &Lab, id: &str) -> Result<Ws> {
    let mut socket = peer_session(
        &lab.source,
        &lab.source_identity,
        &lab.target_identity.device_id,
    )
    .await?;
    ensure!(matches!(
        net::receive::<Data>(&mut socket).await?,
        Data::Ready { .. }
    ));
    net::send(
        &mut socket,
        &Data::Request {
            request: Request::Logs {
                id: id.into(),
                after: 0,
                follow: true,
            },
        },
    )
    .await?;
    ensure!(matches!(
        net::receive::<Data>(&mut socket).await?,
        Data::Logs { .. }
    ));
    Ok(socket)
}

async fn closed(socket: &mut Ws) -> Result<()> {
    loop {
        match net::receive::<Data>(socket).await {
            Err(_) => return Ok(()),
            Ok(Data::Error { .. } | Data::Logs { .. }) => {}
            Ok(message) => anyhow::bail!("unexpected message during invalidation: {message:?}"),
        }
    }
}

async fn invalidate(
    lab: &Lab,
    id: &str,
    name: &str,
    change: impl FnOnce() -> Result<()>,
) -> Result<serde_json::Value> {
    let mut socket = subscription(lab, id).await?;
    change()?;
    // Start after the atomic configuration/roster commit. This excludes CLI
    // startup, delivery of the administrative command and write/fsync latency.
    let started = Instant::now();
    tokio::time::timeout(ACCESS_BUDGET, closed(&mut socket))
        .await
        .with_context(|| format!("{name} did not close the established session within 200 ms"))??;
    Ok(serde_json::json!({"change":name,"closure_ms":started.elapsed().as_secs_f64()*1000.0}))
}

async fn runner(root: &Path) -> Result<std::path::PathBuf> {
    let output = root.join(if cfg!(windows) {
        "task-child.exe"
    } else {
        "task-child"
    });
    let compiled = tokio::process::Command::new("rustc")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/task-child.rs"
        ))
        .args(["--edition=2024", "-o"])
        .arg(&output)
        .output()
        .await?;
    ensure!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    Ok(output)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "controlled idle-machine acceptance; run alone with --ignored --test-threads=1"]
async fn established_sessions_react_within_budgets() -> Result<()> {
    let lab = Lab::new().await?;
    let executable = runner(lab.root.path()).await?;
    let gate = TcpListener::bind("127.0.0.1:0").await?;
    let job = json(
        cli(
            &lab.source,
            &[
                "target1",
                "start",
                "--json",
                "--",
                executable.to_str().context("runner path")?,
                "log-gate",
                &gate.local_addr()?.to_string(),
            ],
        )
        .await,
    );
    let id = job["job_id"].as_str().context("job ID")?;
    let (mut producer, _) = tokio::time::timeout(Duration::from_secs(5), gate.accept()).await??;
    let mut logs = subscription(&lab, id).await?;
    let output_started = Instant::now();
    producer.write_all(&[1]).await?;
    let visible = tokio::time::timeout(OUTPUT_BUDGET, async {
        loop {
            if let Data::Logs { events, .. } = net::receive::<Data>(&mut logs).await?
                && !events.is_empty()
            {
                return Ok::<_, anyhow::Error>(events);
            }
        }
    })
    .await
    .context("triggered output was not visible within 100 ms over established loopback")??;
    let output_ms = output_started.elapsed().as_secs_f64() * 1000.0;
    ensure!(visible[0].stream == "stdout");
    ensure!(STANDARD.decode(&visible[0].data_base64)? == b"gated output\n");
    drop(logs);

    let config_path = lab.target.join(".xrun/daemon.toml");
    let mut results = Vec::new();
    for name in ["deny-from", "pause", "pause-resume"] {
        results.push(
            invalidate(&lab, id, name, || {
                let mut next: DaemonConfig = config::read(&config_path)?;
                if name == "deny-from" {
                    next.deny_from.push(lab.source_identity.device_id.clone());
                } else {
                    next.pause_generation += 1;
                    next.remote_access_paused = true;
                    if name == "pause-resume" {
                        config::write(&config_path, &next)?;
                        next.remote_access_paused = false;
                    }
                }
                config::write(&config_path, &next)
            })
            .await?,
        );
        // An ordinary request refreshes the restored config before opening
        // the next subscription; pause generations never go backwards.
        let mut restored: DaemonConfig = config::read(&config_path)?;
        restored.remote_access_paused = false;
        restored.deny_from.clear();
        config::write(&config_path, &restored)?;
    }
    let manager = Manager::open(&lab.source.join(".xrun/manager"))?;
    let mut renewed = lab.target_identity.clone();
    renewed.cert_pem = manager
        .pair(
            "",
            &renewed.name,
            &crypto::renew_device_request(&renewed.key_pem)?,
        )?
        .cert_pem;
    ensure!(renewed.cert_pem != lab.target_identity.cert_pem);
    results.push(
        invalidate(&lab, id, "certificate-renewal", || {
            config::write(&lab.target.join(".xrun/identity.toml"), &renewed)
        })
        .await?,
    );
    let revoked = manager.revoke(&lab.target_identity.device_id)?;
    let cache = RosterCache::open(&lab.target.join(".xrun/roster.db"))?;
    results.push(
        invalidate(&lab, id, "revocation", || {
            cache.observe(&revoked.roster.network_id, &revoked)
        })
        .await?,
    );
    let report = serde_json::json!({
        "platform":std::env::consts::OS,
        "path":"established encrypted sessions through loopback Rust relay, idle machine",
        "access_budget_ms":200,"output_budget_ms":100,
        "output_trigger_to_remote_visibility_ms":output_ms,"access":results,
    });
    println!("{report}");
    if let Some(path) = std::env::var_os("XRUN_LATENCY_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    drop(producer);
    Ok(())
}
