//! Real CLI/daemon enrollment over a private localhost relay; no external probing.
mod common;
use anyhow::{Context, Result};
use common::*;
use serde_json::Value;
use xrun::testing::config::{self, DaemonConfig, Identity};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn short_codes_are_single_use_and_budgets_survive_relay_reconnect() -> Result<()> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("manager");
    let (mut relay, link) = TestRelay::new(&root.path().join("relay")).await?;
    let manager = json(
        cli(
            &home,
            &[
                "connect",
                &link,
                "--name",
                "manager1",
                "--no-daemon",
                "--json",
            ],
        )
        .await,
    );
    let mut daemon = logged(&home, &["daemon"], "code-manager")?.spawn()?;
    online(&home, "manager1").await?;
    let invitation = json(cli(&home, &["invite", "--json"]).await);
    assert!(
        invitation["code"].is_string(),
        "short invitation fields: {:?}",
        invitation.as_object().map(|v| v.keys().collect::<Vec<_>>())
    );
    let address = invitation["address"].as_str().context("address")?;
    let code = invitation["code"].as_str().context("code")?;
    assert_eq!(code.len(), 9);
    assert!(!address.contains('/'));
    assert!(!invitation["link"].as_str().unwrap().contains("net_"));
    let wrong = format!(
        "{}{}",
        &code[..code.len() - 1],
        if code.ends_with('2') { "3" } else { "2" }
    );
    let wrong_home = root.path().join("wrong");
    for attempt in 0..5 {
        let out = cli(
            &wrong_home,
            &[
                "connect",
                address,
                &wrong,
                "--name",
                "wrong1",
                "--no-daemon",
                "--json",
            ],
        )
        .await;
        assert_eq!(out.status.code(), Some(125), "{attempt}");
        assert_eq!(
            serde_json::from_slice::<Value>(&out.stderr)?["code"],
            "INVALID_CODE"
        );
    }
    relay.stop().await?;
    let process = tokio::spawn(xrun::testing::relay::run(relay.config.clone()));
    online(&home, "manager1").await?;
    let locked = cli(
        &root.path().join("locked"),
        &[
            "connect",
            address,
            code,
            "--name",
            "locked1",
            "--no-daemon",
            "--json",
        ],
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&locked.stderr)?["code"],
        "CODE_LOCKED"
    );
    let invitation = json(cli(&home, &["invite", "--json"]).await);
    let member_home = root.path().join("member");
    let joined = json(
        cli(
            &member_home,
            &[
                "connect",
                invitation["address"].as_str().unwrap(),
                &invitation["code"].as_str().unwrap().to_lowercase(),
                "--name",
                "member1",
                "--no-daemon",
                "--json",
            ],
        )
        .await,
    );
    assert_eq!(joined["network_id"], manager["network_id"]);
    let retry = json(
        cli(
            &member_home,
            &[
                "connect",
                invitation["address"].as_str().unwrap(),
                invitation["code"].as_str().unwrap(),
                "--name",
                "member1",
                "--no-daemon",
                "--json",
            ],
        )
        .await,
    );
    assert_eq!(retry["device_id"], joined["device_id"]);

    let policy: DaemonConfig = config::read(&member_home.join(".xrun/daemon.toml"))?;
    assert!(
        policy
            .deny_from
            .contains(&manager["device_id"].as_str().unwrap().to_string())
    );
    let replay = cli(
        &root.path().join("replay"),
        &[
            "connect",
            invitation["address"].as_str().unwrap(),
            invitation["code"].as_str().unwrap(),
            "--name",
            "replay1",
            "--no-daemon",
            "--json",
        ],
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stderr)?["code"],
        "CODE_IN_USE"
    );
    // A rejected name must not strand the local key when another code is used.
    let collision_home = root.path().join("collision");
    let first = json(cli(&home, &["invite", "--json"]).await);
    let failed = cli(
        &collision_home,
        &[
            "connect",
            first["address"].as_str().unwrap(),
            first["code"].as_str().unwrap(),
            "--name",
            "manager1",
            "--no-daemon",
            "--json",
        ],
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&failed.stderr)?["code"],
        "NAME_IN_USE"
    );
    let second = json(cli(&home, &["invite", "--json"]).await);
    let recovered = json(
        cli(
            &collision_home,
            &[
                "connect",
                second["address"].as_str().unwrap(),
                second["code"].as_str().unwrap(),
                "--name",
                "recovered1",
                "--no-daemon",
                "--json",
            ],
        )
        .await,
    );
    assert_eq!(recovered["name"], "recovered1");
    let old = json(cli(&home, &["invite", "--json"]).await);
    let root_key = std::fs::read(home.join(".xrun/manager/root.key"))?;
    stop_daemon(&home, &mut daemon).await?;
    daemon = logged(&home, &["daemon"], "code-manager-restarted")?.spawn()?;
    online(&home, "manager1").await?;
    let expired = cli(
        &root.path().join("expired"),
        &[
            "connect",
            old["address"].as_str().unwrap(),
            old["code"].as_str().unwrap(),
            "--name",
            "expired1",
            "--no-daemon",
            "--json",
        ],
    )
    .await;
    assert_eq!(
        serde_json::from_slice::<Value>(&expired.stderr)?["code"],
        "CODE_EXPIRED"
    );
    assert_eq!(
        std::fs::read(home.join(".xrun/manager/root.key"))?,
        root_key
    );
    let _: Identity = config::read(&member_home.join(".xrun/identity.toml"))?;
    let full = json(cli(&home, &["invite", "--link", "--json"]).await);
    assert!(full["code"].is_null());
    assert!(full["link"].as_str().unwrap().contains("net_"));
    stop_daemon(&home, &mut daemon).await?;
    process.abort();
    let _ = process.await;
    Ok(())
}
