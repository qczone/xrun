//! Recovery assertions in the shared end-to-end lifecycle.
use super::*;
pub(super) async fn check(suite: &mut Suite) -> Result<()> {
    let Suite {
        source,
        target,
        cfg,
        runner,
        target_daemon,
        ..
    } = suite;
    let runner = runner.to_string_lossy();
    // Desktop Stop must cancel jobs, record the outcome and exit successfully,
    // so supervisors do not immediately relaunch the daemon.
    let graceful = json(
        cli(
            source,
            &["runner1", "start", "--json", "--", &runner, "sleep"],
        )
        .await,
    );
    let graceful_id = graceful["job_id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if json(cli(source, &["runner1", "jobs", graceful_id, "--json"]).await)["state"]
                == "running"
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    assert_eq!(
        json(cli(target, &["status", "--json"]).await)["local"]["daemon_connected"],
        true
    );
    ok(cli(target, &["daemon", "stop"]).await);
    assert!(
        tokio::time::timeout(Duration::from_secs(12), target_daemon.0.wait())
            .await??
            .success()
    );
    assert!(!xrun::testing::config::instance_running(
        &target.join(".xrun/daemon.lock")
    )?);
    *target_daemon = daemon(target);
    online(source, "runner1").await;
    assert_eq!(
        json(cli(source, &["runner1", "jobs", graceful_id, "--json"]).await)["state"],
        "canceled"
    );
    // A crash never replays the intent. The stored PID/start proof cleans up the
    // old process, and its result becomes lost without consuming new capacity.
    let crashed = json(
        cli(
            source,
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
        if json(cli(source, &["runner1", "jobs", crash_id, "--json"]).await)["state"] == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    target_daemon.0.start_kill()?;
    target_daemon.0.wait().await?;
    *target_daemon = daemon(target);
    online(source, "runner1").await;
    let lost = json(cli(source, &["runner1", "jobs", crash_id, "--json"]).await);
    assert_eq!(lost["state"], "lost");
    assert_eq!(
        cli(source, &["runner1", "wait", crash_id])
            .await
            .status
            .code(),
        Some(125)
    );
    let dedup_lost = json(
        cli(
            source,
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
        "SELECT params_json FROM jobs WHERE request_id='dedupe-1'",
        [],
        |r| r.get(0),
    )?;
    assert!(!metadata.contains("private-value"));
    // Reset is explicit and refuses to operate while a daemon is running.
    assert_eq!(
        cli(target, &["daemon", "reset"]).await.status.code(),
        Some(125)
    );
    target_daemon.0.start_kill()?;
    target_daemon.0.wait().await?;
    let db_path = target.join(".xrun/daemon.db");
    drop(task_db);
    std::fs::rename(&db_path, target.join("old-daemon.db"))?;
    assert_eq!(cli(target, &["daemon"]).await.status.code(), Some(125));
    ok(cli(target, &["daemon", "reset"]).await);
    *target_daemon = daemon(target);
    online(source, "runner1").await;
    let reset = cli(
        source,
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
        json(cli(source, &["runner1", "jobs", "--json"]).await),
        serde_json::json!([])
    );

    Ok(())
}
