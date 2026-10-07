//! Access assertions in the shared end-to-end lifecycle.
use super::*;
pub(super) async fn check(suite: &mut Suite) -> Result<()> {
    let Suite {
        source,
        target,
        observer,
        target_id,
        admin,
        invite,
        runner,
        target_daemon,
        ..
    } = suite;
    let runner = runner.to_string_lossy();
    // Whitelist changes are read by the daemon for subsequent requests.
    assert_eq!(
        ok(cli(observer, &["runner1", "--", &runner, "echo", "future"]).await),
        "<future>\n"
    );
    ok(cli(target, &["deny-from", "observer"]).await);
    let refused = cli(observer, &["runner1", "--", &runner, "echo"]).await;
    assert!(String::from_utf8_lossy(&refused.stderr).contains("SOURCE_NOT_ALLOWED"));
    ok(cli(target, &["deny-from", "--all"]).await);
    // Turning off all-member mode preserves the admin's individual grant.
    assert_eq!(
        ok(cli(source, &["runner1", "--", &runner, "echo", "known"]).await),
        "<known>\n"
    );
    assert_eq!(
        cli(target, &["allow-from", "observer", "--all"])
            .await
            .status
            .code(),
        Some(2)
    );

    let completed = json(
        cli(
            source,
            &["runner1", "start", "--json", "--", &runner, "finish-later"],
        )
        .await,
    );
    let completed_id = completed["job_id"].as_str().unwrap();
    let paused_job = json(
        cli(
            source,
            &["runner1", "start", "--json", "--", &runner, "sleep"],
        )
        .await,
    );
    let paused_job_id = paused_job["job_id"].as_str().unwrap();
    let source_identity = xrun::testing::config::read(&source.join(".xrun/identity.toml"))?;
    let mut subscription = common::peer_session(source, &source_identity, target_id).await?;
    assert!(matches!(
        xrun::testing::net::receive::<Data>(&mut subscription).await?,
        Data::Ready { .. }
    ));
    xrun::testing::net::send(
        &mut subscription,
        &Data::Request {
            request: Request::Logs {
                id: paused_job_id.into(),
                after: 0,
                follow: true,
                tail: None,
            },
        },
    )
    .await?;
    assert!(matches!(
        xrun::testing::net::receive::<Data>(&mut subscription).await?,
        Data::Logs { .. }
    ));
    ok(cli(target, &["daemon", "pause"]).await);
    let refused = cli(source, &["runner1", "jobs", "--json"]).await;
    assert_eq!(refused.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("ACCESS_PAUSED"));
    // Even a fast resume invalidates the subscription opened before the pause.
    ok(cli(target, &["daemon", "resume"]).await);
    // Drain in-flight logs and the optional denial response before checking
    // transport closure. Neither a diagnostic nor a timeout proves closure.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match xrun::testing::net::receive::<Data>(&mut subscription).await {
                Ok(Data::Logs { job, .. }) if job.job_id == paused_job_id => {}
                Ok(Data::Error { code, .. })
                    if matches!(code.as_str(), "ACCESS_PAUSED" | "SESSION_CLOSED") => {}
                Ok(message) => anyhow::bail!("unexpected message after pause: {message:?}"),
                Err(_) => return Ok::<_, anyhow::Error>(()),
            }
        }
    })
    .await
    .context("paused log subscription remained open after resume")??;
    ok(cli(target, &["daemon", "pause"]).await);
    let local_tasks =
        xrun::testing::store::TaskStore::open(&target.join(".xrun/daemon.db"), false)?;
    assert!(!local_tasks.get(paused_job_id)?.unwrap().state.terminal());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if local_tasks.get(completed_id).unwrap().unwrap().state == JobState::Exited {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await?;
    // Pause survives restarting the daemon and does not erase task history.
    ok(cli(target, &["daemon", "stop"]).await);
    assert!(
        tokio::time::timeout(Duration::from_secs(12), target_daemon.0.wait())
            .await??
            .success()
    );
    *target_daemon = daemon(target);
    online(source, "runner1").await;
    assert_eq!(
        json(cli(target, &["status", "--json"]).await)["local"]["remote_access_paused"],
        true
    );
    assert_eq!(
        cli(source, &["runner1", "jobs", "--json"])
            .await
            .status
            .code(),
        Some(125)
    );
    ok(cli(target, &["daemon", "resume"]).await);
    assert_eq!(
        json(cli(source, &["runner1", "jobs", completed_id, "--json"]).await)["state"],
        "exited"
    );
    drop(local_tasks);
    ok(cli(target, &["deny-from", admin]).await);
    ok(cli(target, &["join", invite, "--no-daemon"]).await);
    let denied = cli(source, &["runner1", "--", &runner, "echo"]).await;
    assert_eq!(denied.status.code(), Some(125));
    ok(cli(target, &["allow-from", admin]).await);
    online(source, "runner1").await;
    let id_before = std::fs::read_to_string(target.join(".xrun/identity.toml"))?;
    ok(cli(target, &["join", invite, "--no-daemon"]).await);
    let id_after = std::fs::read_to_string(target.join(".xrun/identity.toml"))?;
    assert!(id_before.contains(target_id.as_str()) && id_after.contains(target_id.as_str()));

    Ok(())
}
