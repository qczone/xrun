mod common;
use anyhow::Result;
use common::*;
use std::time::Duration;
use xrun::protocol::{VERSION, sha256};

#[tokio::test]
async fn packaged_binary_contains_the_offline_manual_and_remote_help() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        ok(cli(home.path(), &["doc"]).await),
        include_str!("../docs/usage.md")
    );
    let help = ok(cli(home.path(), &["help", "push"]).await);
    assert!(help.contains("<LOCAL> <REMOTE>") && help.contains("--expect"));
    let help = ok(cli(home.path(), &["relay", "deploy", "--help"]).await);
    assert!(
        help.contains("--provider")
            && help.contains("--name")
            && help.contains("--account")
            && help.contains("--dry-run")
            && help.contains("--addr")
    );
    if std::env::var_os("XRUN_CLOUDFLARE_BUNDLE_DIR").is_some() {
        let result = json(
            command(
                home.path(),
                &[
                    "relay",
                    "deploy",
                    "--provider",
                    "cloudflare",
                    "--dry-run",
                    "--json",
                ],
            )
            .env_remove("XRUN_CLOUDFLARE_OAUTH_CLIENT_ID")
            .output()
            .await
            .unwrap(),
        );
        assert_eq!(result["version"], VERSION);
        assert_eq!(result["bundled"], true);
        assert_eq!(result["sha256"].as_str().unwrap().len(), 64);
        assert_eq!(result["oauth_configured"], true);
    }
}

#[tokio::test]
async fn relay_providers_and_specific_options_are_checked_before_any_side_effects() {
    let home = tempfile::tempdir().unwrap();
    let output = cli(home.path(), &["relay", "deploy"]).await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--provider"));
    for args in [
        vec![
            "relay",
            "deploy",
            "--provider",
            "cloudflare",
            "--port",
            "9528",
        ],
        vec!["relay", "deploy", "--provider", "local", "--name", "worker"],
        vec!["relay", "remove", "--provider", "cloudflare"],
        vec!["relay", "remove", "--provider", "local", "--purge"],
        vec!["relay", "login", "--provider", "local"],
        vec!["relay", "run", "--provider", "cloudflare"],
    ] {
        let output = cli(home.path(), &args).await;
        assert!(!output.status.success(), "{args:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("INVALID_REQUEST"),
            "{args:?}"
        );
    }
    assert!(!home.path().join(".xrun").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn packaged_binary_creates_joins_executes_and_transfers_files() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(45), async {
        println!("smoke binary: {}", binary().display());
        let lab = Lab::new().await?;
        let program = binary().to_string_lossy().into_owned();
        assert_eq!(
            ok(cli(&lab.source, &["target1", "--", &program, "--version"]).await).trim(),
            format!("xrun {VERSION}")
        );
        let mut content = b"\xef\xbb\xbfartifact smoke\r\n".to_vec();
        content.extend((0..1_100_000).map(|i| (i % 256) as u8));
        let local = lab.source.join("upload.bin");
        let remote = lab.target.join("artifact.bin");
        let download = lab.source.join("download.bin");
        std::fs::write(&local, &content)?;
        ok(cli(
            &lab.source,
            &[
                "target1",
                "push",
                &local.to_string_lossy(),
                &remote.to_string_lossy(),
                "--no-overwrite",
            ],
        )
        .await);
        let pulled = json(
            cli(
                &lab.source,
                &[
                    "target1",
                    "pull",
                    &remote.to_string_lossy(),
                    &download.to_string_lossy(),
                    "--json",
                ],
            )
            .await,
        );
        assert_eq!(pulled["sha256"], sha256(&content));
        assert_eq!(sha256(&std::fs::read(&download)?), sha256(&content));
        assert_eq!(std::fs::read(download)?.len(), content.len());
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
