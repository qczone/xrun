mod common;
use anyhow::Result;
use common::*;
use xrun::testing::{crypto, net, protocol::RelayMessage};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn first_manager_claim_survives_restart_and_other_devices_need_an_invitation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (mut relay, link) = TestRelay::new(&root.path().join("relay")).await?;
    let addresses = xrun::testing::relay::addresses(&relay.config)?;
    let ca = crypto::load_or_create_server(&relay.config)?.ca_pem;
    let mut probe = net::websocket_at(
        &addresses[0],
        "/networks/probe/status",
        crypto::relay_tls_config(&ca)?,
    )
    .await?;
    assert!(matches!(
        net::receive(&mut probe).await?,
        RelayMessage::Challenge { .. }
    ));
    assert!(matches!(
        net::receive(&mut probe).await?,
        RelayMessage::RelayInfo { owner: None }
    ));
    assert!(!relay.config.data_dir.join("owner.json").exists());
    drop(probe);

    let first = root.path().join("first");
    let second = root.path().join("second");
    let first_args = [
        "up",
        "--relay",
        &link,
        "--name",
        "first",
        "--no-daemon",
        "--json",
    ];
    let second_args = [
        "up",
        "--relay",
        &link,
        "--name",
        "second",
        "--no-daemon",
        "--json",
    ];
    let (a, b) = tokio::join!(cli(&first, &first_args), cli(&second, &second_args));
    assert_ne!(a.status.success(), b.status.success());
    let member = if a.status.success() {
        second.clone()
    } else {
        first.clone()
    };
    let (home, name, winner, loser) = if a.status.success() {
        (&first, "first", a, b)
    } else {
        (&second, "second", b, a)
    };
    let winner = json(winner);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&loser.stderr)?["code"],
        "RELAY_CLAIMED"
    );
    let owner_path = relay.config.data_dir.join("owner.json");
    let owner = std::fs::read(&owner_path)?;
    let record: serde_json::Value = serde_json::from_slice(&owner)?;
    assert_eq!(record["manager_id"], winner["device_id"]);
    assert_eq!(record["network_id"], winner["network_id"]);
    let retry = json(cli(home, &["up", "--relay", &link, "--no-daemon", "--json"]).await);
    assert_eq!(retry["device_id"], winner["device_id"]);
    assert_eq!(std::fs::read(&owner_path)?, owner);

    relay.stop().await?;
    relay.start().await?;
    let stranger = root.path().join("stranger");
    let denied = cli(
        &stranger,
        &["up", "--relay", &link, "--no-daemon", "--json"],
    )
    .await;
    assert_eq!(denied.status.code(), Some(125));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&denied.stderr)?["code"],
        "RELAY_CLAIMED"
    );
    assert!(!stranger.join(".xrun/identity.toml").exists());
    assert!(!stranger.join(".xrun/manager").exists());
    assert_eq!(std::fs::read(&owner_path)?, owner);

    let mut manager = logged(home, &["daemon"], "manager")?
        .kill_on_drop(true)
        .spawn()?;
    online(home, name).await?;
    let invitation = json(cli(home, &["invite", "--json"]).await);
    let joined = json(
        cli(
            &member,
            &[
                "join",
                invitation["link"].as_str().unwrap(),
                "--name",
                "member",
                "--no-daemon",
                "--json",
            ],
        )
        .await,
    );
    let identity: xrun::testing::config::Identity =
        xrun::testing::config::read(&member.join(".xrun/identity.toml"))?;
    assert_eq!(joined["device_id"], identity.device_id);
    assert_eq!(identity.network.unwrap().network_id, winner["network_id"]);
    assert_eq!(std::fs::read(&owner_path)?, owner);
    manager.start_kill()?;
    manager.wait().await?;
    relay.stop().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn moving_to_another_owned_relay_preserves_the_original_identity_and_roster() -> Result<()> {
    let mut lab = Lab::new().await?;
    lab.source_daemon.start_kill()?;
    lab.source_daemon.wait().await?;
    let home = lab.root.path().join("other-manager");
    let (mut relay, link) = TestRelay::new(&lab.root.path().join("other-relay")).await?;
    json(
        cli(
            &home,
            &[
                "up",
                "--relay",
                &link,
                "--name",
                "other",
                "--no-daemon",
                "--json",
            ],
        )
        .await,
    );
    let path = lab.source.join(".xrun/identity.toml");
    let identity = std::fs::read(&path)?;
    let manager = xrun::testing::membership::Manager::open(&lab.source.join(".xrun/manager"))?;
    let roster = manager.roster()?.roster;
    let denied = cli(
        &lab.source,
        &["up", "--relay", &link, "--no-daemon", "--json"],
    )
    .await;
    assert_eq!(denied.status.code(), Some(125));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&denied.stderr)?["code"],
        "RELAY_CLAIMED"
    );
    assert_eq!(std::fs::read(&path)?, identity);
    assert_eq!(manager.roster()?.roster, roster);
    relay.stop().await?;
    Ok(())
}
