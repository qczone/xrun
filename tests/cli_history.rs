mod common;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use std::{process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;

async fn script(lab: &Lab, text: &str) -> Result<String> {
    let shell = if cfg!(windows) { "powershell" } else { "sh" };
    let mut child = command(&lab.source, &["target1", "start", "--script", shell])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(text.as_bytes())
        .await?;
    Ok(ok(child.wait_with_output().await?).trim().into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn history_text_json_tail_and_wait_timeouts_preserve_job_results() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut lab = Lab::new().await?;
        let invalid = cli(
            &lab.source,
            &["target1", "--script", "unknown-shell", "--json"],
        )
        .await;
        assert_eq!(invalid.status.code(), Some(2));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&invalid.stderr)?["code"],
            "INVALID_SHELL"
        );
        let failed = cli(
            &lab.source,
            &["target1", "--", "xrun-test-program-that-does-not-exist"],
        )
        .await;
        assert_eq!(failed.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&failed.stderr).contains("PROGRAM_NOT_FOUND"));
        #[cfg(unix)]
        assert_eq!(
            cli(
                &lab.source,
                &["target1", "-i", "--", "/bin/sh", "-c", "kill -TERM $$"]
            )
            .await
            .status
            .code(),
            Some(143)
        );
        let text = if cfg!(windows) {
            "Write-Output first; Write-Output second; Write-Output third; exit 7"
        } else {
            "printf 'first\nsecond\nthird\n'; exit 7"
        };
        let reference = script(&lab, text).await?;
        let out = cli(
            &lab.source,
            &[
                "target1",
                "wait",
                &reference,
                "--timeout",
                "5",
                "--tail",
                "0",
                "--json",
            ],
        )
        .await;
        assert_eq!(out.status.code(), Some(7));
        let result: serde_json::Value = serde_json::from_slice(&out.stdout)?;
        assert_eq!(result["job"]["exit_code"], 7);
        assert_eq!(result["logs"], serde_json::json!([]));
        assert!(result["logs_error"].is_null());
        let jobs = ok(cli(&lab.source, &["target1", "jobs"]).await);
        assert!(jobs.contains(reference.split_once('/').unwrap().1));
        assert!(jobs.contains("Exited"));
        for follow in [false, true] {
            let mut args = vec!["target1", "logs", &reference, "--tail", "2"];
            if follow {
                args.push("--follow");
            }
            let logs = ok(cli(&lab.source, &args).await).replace("\r\n", "\n");
            assert_eq!(logs, "second\nthird\n");
        }
        let logs = json(
            cli(
                &lab.source,
                &["target1", "logs", &reference, "--tail", "1", "--json"],
            )
            .await,
        );
        let bytes = logs
            .as_array()
            .context("logs")?
            .iter()
            .flat_map(|event| {
                STANDARD
                    .decode(event["data_base64"].as_str().unwrap())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(String::from_utf8(bytes)?.replace("\r\n", "\n"), "third\n");
        let recent = ok(cli(&lab.source, &["recent"]).await);
        assert!(recent.contains("confirmed") && recent.contains(&lab.target_identity.device_id));

        for op in ["jobs", "wait", "logs", "kill"] {
            let out = cli(
                &lab.source,
                &["target1", op, "wrong-device/ABC123", "--json"],
            )
            .await;
            assert_eq!(out.status.code(), Some(2));
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&out.stderr)?["code"],
                "INVALID_JOB_REF"
            );
        }
        let missing = cli(&lab.source, &["target1", "wait", "000000", "--json"]).await;
        assert_eq!(missing.status.code(), Some(75));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&missing.stderr)?["code"],
            "JOB_NOT_FOUND"
        );
        let missing = cli(&lab.source, &["target1", "kill", "000000", "--json"]).await;
        assert_eq!(missing.status.code(), Some(125));

        let long = script(
            &lab,
            if cfg!(windows) {
                "Start-Sleep -Seconds 60"
            } else {
                "sleep 60"
            },
        )
        .await?;
        let out = cli(
            &lab.source,
            &["target1", "wait", &long, "--timeout", "1", "--json"],
        )
        .await;
        assert_eq!(out.status.code(), Some(75));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&out.stderr)?["code"],
            "WAIT_TIMEOUT"
        );
        let running = json(cli(&lab.source, &["target1", "jobs", &long, "--json"]).await);
        assert!(matches!(
            running["state"].as_str(),
            Some("starting" | "running")
        ));
        ok(cli(&lab.source, &["target1", "kill", &long]).await);
        let stopped = cli(&lab.source, &["target1", "wait", &long, "--tail", "0"]).await;
        assert_eq!(stopped.status.code(), Some(130));
        ok(cli(&lab.target, &["deny-from", "source1"]).await);
        let denied = cli(&lab.source, &["target1", "wait", &long, "--json"]).await;
        assert_eq!(denied.status.code(), Some(125));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&denied.stderr)?["code"],
            "SOURCE_NOT_ALLOWED"
        );
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_text_distinguishes_unjoined_paused_revoked_and_unavailable_devices() -> Result<()> {
    let fresh = tempfile::tempdir()?;
    let status = ok(cli(fresh.path(), &["status"]).await);
    assert!(status.contains("local: not joined") && status.contains("daemon: stopped"));
    assert_eq!(
        ok(cli(fresh.path(), &["guide"]).await),
        include_str!("../README.md")
    );
    let mut lab = Lab::new().await?;
    let status = ok(cli(&lab.source, &["status"]).await);
    assert!(status.contains("local: source1") && status.contains("daemon: running"));
    assert!(status.contains("relay-connected"));
    ok(cli(&lab.source, &["allow-from", "--all"]).await);
    ok(cli(&lab.source, &["daemon", "pause"]).await);
    let status = ok(cli(&lab.source, &["status"]).await);
    assert!(
        status.contains("remote access: paused")
            && status.contains("all current and future members")
    );
    ok(cli(&lab.source, &["daemon", "resume"]).await);
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    let status = ok(cli(&lab.source, &["status"]).await);
    assert!(
        status
            .lines()
            .any(|line| line.starts_with("target1\t") && line.ends_with("disconnected"))
    );
    let revoked = cli(&lab.source, &["revoke", "target1"]).await;
    assert!(ok(revoked).contains("revoked"));
    let status = ok(cli(&lab.source, &["status"]).await);
    assert!(
        status
            .lines()
            .any(|line| line.starts_with("target1\t") && line.ends_with("revoked"))
    );
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    lab.relay.stop().await?;
    let status = cli(&lab.source, &["status"]).await;
    assert_eq!(status.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&status.stdout).contains("daemon: stopped"));
    assert!(String::from_utf8_lossy(&status.stderr).contains("device list unavailable"));
    Ok(())
}
