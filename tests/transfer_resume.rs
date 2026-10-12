mod common;
use anyhow::{Context, Result};
use common::*;
use std::{path::Path, process::Stdio, time::Duration};
use xrun::testing::{protocol::JobState, store::JobStore};
fn checkpoint(home: &Path) -> Result<Option<(String, serde_json::Value)>> {
    let base = home.join(".xrun/client-transfers");
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().is_some_and(|ext| ext == "json") {
            let value: serde_json::Value = serde_json::from_slice(&std::fs::read(entry.path())?)?;
            if value["offset"].as_u64().unwrap_or(0) > 0 {
                return Ok(Some((
                    entry.path().file_stem().unwrap().to_string_lossy().into(),
                    value,
                )));
            }
        }
    }
    Ok(None)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn equal_files_skip_payload_and_still_create_distinct_activity_jobs() -> Result<()> {
    let lab = Lab::new().await?;
    let local = lab.source.join("same.txt");
    let remote = lab.target.join("same.txt");
    let content = b"owned equal file\n";
    std::fs::write(&local, content)?;
    std::fs::write(&remote, content)?;
    let uploaded = json(
        cli(
            &lab.source,
            &[
                "target1",
                "push",
                local.to_str().unwrap(),
                remote.to_str().unwrap(),
                "--json",
            ],
        )
        .await,
    );
    assert_eq!(uploaded["skipped"], true);
    let pulled = json(
        cli(
            &lab.source,
            &[
                "target1",
                "pull",
                remote.to_str().unwrap(),
                local.to_str().unwrap(),
                "--json",
            ],
        )
        .await,
    );
    assert_eq!(pulled["skipped"], true);
    assert_ne!(uploaded["request_id"], pulled["request_id"]);
    let store = JobStore::open(&lab.target.join(".xrun/daemon.db"), false)?;
    let records = store.page(&lab.source_identity.device_id, false, None, 10, 0)?;
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|job| job.state == JobState::Succeeded && job.attachments.len() == 1)
    );
    assert_eq!(std::fs::read(local)?, content);
    Ok(())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn interrupted_upload_resumes_its_durable_intent_after_target_restart_and_source_change()
-> Result<()> {
    tokio::time::timeout(Duration::from_secs(100), async {
        let mut lab = Lab::new().await?;
        let local = lab.source.join("large.bin");
        let remote = lab.target.join("large.bin");
        let file = std::fs::File::create(&local)?;
        file.set_len(64 * 1024 * 1024 + 123)?;
        file.sync_all()?;
        drop(file);
        let (_, size, hash) = xrun::testing::transfer::prepare_upload(&local)?;
        let mut child = command(
            &lab.source,
            &[
                "target1",
                "push",
                local.to_str().unwrap(),
                remote.to_str().unwrap(),
                "--json",
            ],
        )
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
        let (request_id, saved) = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if let Some(checkpoint) = checkpoint(&lab.source)? {
                    break Ok::<_, anyhow::Error>(checkpoint);
                }
                anyhow::ensure!(
                    child.try_wait()?.is_none(),
                    "fixture upload finished before interruption"
                );
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await??;
        child.start_kill()?;
        let _ = child.wait().await?;
        let store = JobStore::open(&lab.target.join(".xrun/daemon.db"), false)?;
        let before = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(job) = store.by_request(&lab.source_identity.device_id, &request_id)?
                    && job.state == JobState::Interrupted
                {
                    break Ok::<_, anyhow::Error>(job);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await??;
        assert!(saved["offset"].as_u64().unwrap() > 0);
        assert!(!remote.exists(), "partial content must never be published");
        lab.daemon.start_kill()?;
        let _ = lab.daemon.wait().await?;
        lab.daemon = logged(&lab.target, &["daemon"], "resume-target-restarted")?.spawn()?;
        online(&lab.source, "target1").await?;
        std::fs::write(&local, b"new content is another intent")?;
        let resumed = json(cli(&lab.source, &["target1", "resume", &request_id, "--json"]).await);
        assert_eq!(resumed["request_id"], request_id);
        assert_eq!(
            resumed["job"],
            format!("{}/{}", lab.target_identity.device_id, before.job_id)
        );
        assert!(resumed["resumed_from"].as_u64().unwrap() > 0);
        assert_eq!(resumed["size"], size);
        assert_eq!(resumed["sha256"], hash);
        let (_, received, actual) = xrun::testing::transfer::prepare_upload(&remote)?;
        assert_eq!((received, actual), (size, hash));
        let after = store.get(&before.job_id)?.context("original file job")?;
        assert_eq!(after.state, JobState::Succeeded);
        assert_eq!(after.created_at_ms, before.created_at_ms);
        assert_eq!(
            store
                .page(&lab.source_identity.device_id, false, None, 10, 0)?
                .len(),
            1
        );
        assert_eq!(after.attachments.len(), 1);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
