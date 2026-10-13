mod common;
use anyhow::Result;
use common::*;

#[tokio::test]
async fn agent_deployment_output_is_json_and_readiness_checks_do_not_claim_owned_relays()
-> Result<()> {
    let home = tempfile::tempdir()?;
    let command = cli(
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
    .await;
    let result = json(command.clone());
    assert_eq!(result["bundled"], true);
    for line in String::from_utf8(command.stderr)?.lines() {
        let event: serde_json::Value = serde_json::from_str(line)?;
        assert_eq!(event["event"], "deployment_progress");
    }
    assert!(
        !home.path().join(".xrun").exists(),
        "dry-run must not create profile or log files"
    );
    let missing = cli(home.path(), &["relay", "deploy", "--json"]).await;
    assert_eq!(missing.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&missing.stderr)?;
    assert_eq!(error["code"], "INVALID_REQUEST");
    assert!(!home.path().join(".xrun").exists());
    let (_relay, link) = TestRelay::new(&home.path().join("relay")).await?;
    xrun::client::relay::check_relay(&link).await?;
    let (address, _) = link.rsplit_once('/').unwrap();
    let mismatched = format!("{address}/{}", "a".repeat(52));
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        xrun::client::relay::check_relay(&mismatched),
    )
    .await?
    .unwrap_err();
    assert!(xrun::error::is(
        &error,
        xrun::error::ErrorCode::InvalidCertificate
    ));
    // The probe must leave the relay unclaimed, so a regular first creation still succeeds.
    let manager = home.path().join("manager");
    std::fs::create_dir(&manager)?;
    let created = cli(
        &manager,
        &[
            "up",
            "--relay",
            &link,
            "--name",
            "owner1",
            "--no-daemon",
            "--json",
        ],
    )
    .await;
    let network = json(created.clone());
    assert_eq!(network["expires_in"], 600);
    for line in String::from_utf8(created.stderr)?.lines() {
        let _: serde_json::Value = serde_json::from_str(line)?;
    }
    xrun::client::relay::check_relay(&link).await?;
    let failed = cli(
        home.path(),
        &[
            "relay",
            "deploy",
            "--provider",
            "cloudflare",
            "--name",
            "INVALID",
            "--json",
        ],
    )
    .await;
    assert!(!failed.status.success());
    for line in String::from_utf8(failed.stderr)?.lines() {
        let _: serde_json::Value = serde_json::from_str(line)?;
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_headless_authorization_returns_130_without_creating_a_network() -> Result<()> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let home = tempfile::tempdir()?;
    let mut process = command(
        home.path(),
        &[
            "relay",
            "deploy",
            "--provider",
            "cloudflare",
            "--no-browser",
            "--json",
        ],
    );
    process
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("XRUN_CLOUDFLARE_OAUTH_CLIENT_ID");
    process
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = process.spawn()?;
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let line = lines
                .next_line()
                .await?
                .ok_or_else(|| anyhow::anyhow!("authorization exited before browser handoff"))?;
            let event: serde_json::Value = serde_json::from_str(&line)?;
            if event["stage"] == "authorization" && event["status"] == "waiting" {
                assert!(
                    event["authorization_url"]
                        .as_str()
                        .unwrap()
                        .starts_with("https://dash.cloudflare.com/oauth2/auth?")
                );
                break;
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    unsafe {
        assert_eq!(libc::kill(child.id().unwrap() as i32, libc::SIGINT), 0);
    }
    let status = tokio::time::timeout(std::time::Duration::from_secs(10), child.wait()).await??;
    assert_eq!(status.code(), Some(130));
    assert!(!home.path().join(".xrun/identity.toml").exists());
    Ok(())
}
