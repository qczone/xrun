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
