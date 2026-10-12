//! Real CLI/daemon enrollment over a private localhost relay; no external probing.
mod common;
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use spake2::{Ed25519Group, Identity as PakeIdentity, Password, Spake2};
use tokio_tungstenite::tungstenite::Message;
use xrun::testing::config::{self, DaemonConfig, Identity};

async fn raw_handshake(home: &std::path::Path, invitation: &Value, malformed: bool) -> Result<()> {
    use xrun::testing::{crypto, membership::RosterCache, net, protocol::RelayMessage};
    let id: Identity = config::read(&home.join(".xrun/identity.toml"))?;
    let roster = RosterCache::open(&home.join(".xrun/roster.db"))?
        .load(&id.network.as_ref().unwrap().network_id)?;
    let address = format!("https://{}", invitation["address"].as_str().unwrap());
    let locator = &invitation["code"].as_str().unwrap()[..2];
    let mut ws = net::websocket_at(
        &address,
        &format!("/.well-known/xrun/pair/{locator}"),
        crypto::anonymous_tls_config(&roster.roster.relay_ca_pem)?,
    )
    .await?;
    assert!(matches!(
        net::receive(&mut ws).await?,
        RelayMessage::PairingInfo { .. }
    ));
    assert!(matches!(
        net::receive(&mut ws).await?,
        RelayMessage::Challenge { .. }
    ));
    net::send(&mut ws, &RelayMessage::Authenticate { proof: None }).await?;
    assert!(matches!(
        net::receive(&mut ws).await?,
        RelayMessage::Connected { .. }
    ));
    let (_, message) = Spake2::<Ed25519Group>::start_a(
        &Password::new(b"test-only-guess"),
        &PakeIdentity::new(b"joiner"),
        &PakeIdentity::new(b"manager"),
    );
    let csr = if malformed {
        b"malformed".to_vec()
    } else {
        crypto::new_device_request()?.1
    };
    ws.send(Message::Binary(
        serde_json::to_vec(&serde_json::json!({
            "pairing":"hello", "invitation_id":invitation["invitation_id"], "origin":address,
            "csr":STANDARD.encode(csr), "message":STANDARD.encode(message),
        }))?
        .into(),
    ))
    .await?;
    loop {
        if let Message::Binary(bytes) = ws.next().await.context("pairing response")?? {
            let response: Value = serde_json::from_slice(&bytes)?;
            assert_eq!(
                response["pairing"],
                if malformed { "error" } else { "challenge" }
            );
            break;
        }
    }
    // Abandon the valid exchange before Confirm: the attempt must still count.
    ws.close(None).await?;
    Ok(())
}

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
    let probe = json(cli(&home, &["invite", "--json"]).await);
    raw_handshake(&home, &probe, true).await?;
    let attempts = || -> Result<i64> {
        let db = rusqlite::Connection::open(home.join(".xrun/manager/manager.db"))?;
        Ok(db.query_row(
            "SELECT attempts FROM invitations WHERE invitation_id=?1",
            [probe["invitation_id"].as_str().unwrap()],
            |row| row.get(0),
        )?)
    };
    assert_eq!(attempts()?, 0);
    raw_handshake(&home, &probe, false).await?;
    assert_eq!(attempts()?, 1);
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
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(1_050 << (attempt - 1))).await;
        }
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
