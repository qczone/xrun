//! Public CLI authority operations against owned localhost profiles only.
mod common;
use anyhow::Result;
use common::*;
use std::{path::Path, process::Stdio};
use tokio::io::AsyncWriteExt;
use xrun::testing::config;

async fn command(home: &Path, args: &[&str], password: &str) -> std::process::Output {
    let mut child = common::command(home, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(password.as_bytes()).await.unwrap();
    drop(stdin);
    child.wait_with_output().await.unwrap()
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_stdin_exports_without_publishing_secrets_and_restore_does_not_start_service()
-> Result<()> {
    let lab = Lab::new().await?;
    let archive = lab.root.path().join("network.xrun-backup");
    let exported = json(
        command(
            &lab.source,
            &[
                "network",
                "backup",
                archive.to_str().unwrap(),
                "--password-stdin",
                "--json",
            ],
            "owned-fixture-backup-password\n",
        )
        .await,
    );
    assert_eq!(exported["manager_id"], lab.source_identity.device_id);
    assert!(exported.get("root_key").is_none());
    let restored = lab.root.path().join("restored-profile");
    let result = json(
        command(
            &restored,
            &[
                "network",
                "restore",
                archive.to_str().unwrap(),
                "--password-stdin",
                "--json",
            ],
            "owned-fixture-backup-password\n",
        )
        .await,
    );
    assert_eq!(result["authority_generation"], 2);
    assert!(!config::instance_running(
        &restored.join(".xrun/daemon.lock")
    )?);
    let status = json(cli(&restored, &["status", "--json"]).await);
    assert_eq!(status["local"]["device_id"], lab.source_identity.device_id);
    assert_eq!(status["local"]["joined"], true);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn handover_publishes_a_new_generation_and_retires_the_old_manager() -> Result<()> {
    let mut lab = Lab::new().await?;
    let archive = lab.root.path().join("handover.xrun-backup");
    json(
        command(
            &lab.source,
            &[
                "network",
                "migrate",
                "target1",
                "--out",
                archive.to_str().unwrap(),
                "--password-stdin",
                "--json",
            ],
            "owned-fixture-backup-password\n",
        )
        .await,
    );
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    let imported = json(
        command(
            &lab.target,
            &[
                "network",
                "restore",
                archive.to_str().unwrap(),
                "--password-stdin",
                "--json",
            ],
            "owned-fixture-backup-password\n",
        )
        .await,
    );
    assert_eq!(imported["manager_id"], lab.target_identity.device_id);
    lab.daemon = logged(&lab.target, &["daemon"], "new-manager")?.spawn()?;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if !lab.source.join(".xrun/manager/root.key").exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await?;
    let status = json(cli(&lab.source, &["status", "--json"]).await);
    let manager = status["devices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["device_id"] == lab.target_identity.device_id)
        .unwrap();
    assert_eq!(manager["admin"], true);
    assert_eq!(manager["online"], true);
    let denied = cli(
        &lab.target,
        &["source1", "--", "xrun-owned-fixture-command-does-not-exist"],
    )
    .await;
    assert!(!denied.status.success());
    assert!(!lab.source.join(".xrun/manager/root.key").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn old_backup_merges_relay_revocations_before_replacing_the_management_generation()
-> Result<()> {
    let mut lab = Lab::new().await?;
    let archive = lab.root.path().join("older.xrun-backup");
    json(
        command(
            &lab.source,
            &[
                "network",
                "backup",
                archive.to_str().unwrap(),
                "--password-stdin",
                "--json",
            ],
            "owned-fixture-backup-password\n",
        )
        .await,
    );
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    json(cli(&lab.source, &["revoke", "target1", "--json"]).await);
    let restored = lab.root.path().join("restored-manager");
    let info = json(
        command(
            &restored,
            &[
                "network",
                "restore",
                archive.to_str().unwrap(),
                "--password-stdin",
                "--json",
            ],
            "owned-fixture-backup-password\n",
        )
        .await,
    );
    assert_eq!(info["authority_generation"], 2);
    let mut daemon = logged(&restored, &["daemon"], "restored-manager")?.spawn()?;
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        while lab.source.join(".xrun/manager/root.key").exists() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await?;
    let status = json(cli(&restored, &["status", "--json"]).await);
    assert!(
        status["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |member| member["device_id"] == lab.target_identity.device_id
                    && member["revoked"] == true
            )
    );
    assert!(
        !cli(&lab.source, &["invite", "--json"])
            .await
            .status
            .success()
    );
    stop_daemon(&restored, &mut daemon).await?;
    Ok(())
}
