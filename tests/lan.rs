mod common;
use anyhow::{Context, Result};
use common::*;
use std::time::Duration;
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opt_in_direct_channels_execute_and_fall_back_before_submission_when_disabled() -> Result<()>
{
    tokio::time::timeout(Duration::from_secs(35), async {
        let lab = Lab::new().await?;
        for profile in [&lab.target, &lab.source] {
            ok(cli(profile, &["network", "direct", "127.0.0.1"]).await);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        let program = binary();
        let out = ok(cli(
            &lab.source,
            &["target1", "--", program.to_str().unwrap(), "--version"],
        )
        .await);
        assert!(out.contains("xrun"));
        let routes: serde_json::Value = serde_json::from_slice(&std::fs::read(
            lab.source.join(".xrun/connection-routes.json"),
        )?)?;
        assert_eq!(routes[&lab.target_identity.device_id], "direct");
        let jobs = json(cli(&lab.source, &["target1", "jobs", "--json"]).await);
        assert_eq!(jobs.as_array().context("jobs")?.len(), 1);
        for profile in [&lab.source, &lab.target] {
            ok(cli(profile, &["network", "compression", "none"]).await);
        }
        let before = json(cli(&lab.source, &["traffic", "--period", "all", "--json"]).await);
        let file = lab.source.join("direct.bin");
        std::fs::write(&file, vec![7; 4 * 1024 * 1024])?;
        ok(cli(
            &lab.source,
            &[
                "target1",
                "push",
                file.to_str().unwrap(),
                lab.target.join("direct.bin").to_str().unwrap(),
            ],
        )
        .await);
        let after = json(cli(&lab.source, &["traffic", "--period", "all", "--json"]).await);
        let delta = after["totals"]["egress_bytes"].as_u64().unwrap()
            - before["totals"]["egress_bytes"].as_u64().unwrap();
        assert!(
            delta < 64 * 1024,
            "LAN payload must not inflate relay counters: {delta}"
        );
        ok(cli(&lab.target, &["network", "direct", "--off"]).await);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let out = ok(cli(
            &lab.source,
            &["target1", "--", program.to_str().unwrap(), "--version"],
        )
        .await);
        assert!(out.contains("xrun"));
        let routes: serde_json::Value = serde_json::from_slice(&std::fs::read(
            lab.source.join(".xrun/connection-routes.json"),
        )?)?;
        assert_eq!(routes[&lab.target_identity.device_id], "relay");
        let jobs = json(cli(&lab.source, &["target1", "jobs", "--json"]).await);
        assert_eq!(
            jobs.as_array().unwrap().len(),
            3,
            "fallback must not submit twice"
        );
        let invalid = cli(&lab.target, &["network", "direct", "8.8.8.8", "--json"]).await;
        assert!(!invalid.status.success());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
