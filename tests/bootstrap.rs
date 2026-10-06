mod common;
use anyhow::Result;
use common::*;
use xrun::testing::{
    config::{self, Identity},
    crypto,
    membership::Manager,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn initial_creation_recovers_only_matching_pending_authority() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (mut relay, link) = TestRelay::new(&root.path().join("relay")).await?;
    let ca = std::fs::read_to_string(relay.config.data_dir.join("ca.pem"))?;
    let addresses = xrun::testing::relay::addresses(&relay.config)?;
    for case in [
        "missing-key",
        "wrong-key",
        "wrong-cert",
        "wrong-name",
        "wrong-relay",
        "progressed",
        "valid",
    ] {
        let home = root.path().join(case);
        let dir = home.join(".xrun/manager");
        let (manager, member, key, cert) = Manager::create(
            &dir,
            "manager1",
            if case == "wrong-relay" {
                vec!["https://127.0.0.1:1".into()]
            } else {
                addresses.clone()
            },
            ca.clone(),
        )?;
        let before = manager.roster()?;
        let expected = match case {
            "missing-key" => {
                std::fs::remove_file(dir.join("device.key.pending"))?;
                Some("MANAGER_STATE_MISSING")
            }
            "wrong-key" => {
                config::atomic_private_write(
                    &dir.join("device.key.pending"),
                    crypto::new_device_request()?.0.as_bytes(),
                )?;
                Some("MANAGER_STATE_MISMATCH")
            }
            "wrong-cert" => {
                let (_, _, _, other) = Manager::create(
                    &root.path().join("other"),
                    "other1",
                    addresses.clone(),
                    ca.clone(),
                )?;
                config::atomic_private_write(&dir.join("device.pem.pending"), other.as_bytes())?;
                Some("MANAGER_STATE_MISMATCH")
            }
            "progressed" => {
                manager.pair(
                    &manager.invite(false)?,
                    "peer1",
                    &crypto::new_device_request()?.1,
                )?;
                Some("MANAGER_STATE_EXISTS")
            }
            "wrong-name" | "wrong-relay" => Some("MANAGER_STATE_MISMATCH"),
            _ => None,
        };
        let authority_key = std::fs::read(dir.join("root.key"))?;
        let saved = manager.roster()?;
        let out = cli(
            &home,
            &[
                "up",
                "--relay",
                &link,
                "--name",
                if case == "wrong-name" {
                    "renamed"
                } else {
                    "manager1"
                },
                "--no-daemon",
                "--json",
            ],
        )
        .await;
        if let Some(expected) = expected {
            assert_eq!(out.status.code(), Some(125), "{case}");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&out.stderr)?["code"],
                expected,
                "{case}"
            );
            assert!(!home.join(".xrun/identity.toml").exists());
            assert_eq!(manager.roster()?.roster, saved.roster);
        } else {
            let out = json(out);
            assert_eq!(out["device_id"], member.device_id);
            let id: Identity = config::read(&home.join(".xrun/identity.toml"))?;
            assert_eq!(id.key_pem, key);
            assert_eq!(id.cert_pem, cert);
            assert!(!dir.join("device.key.pending").exists());
            assert!(!dir.join("device.pem.pending").exists());
            let retry = json(cli(&home, &["up", "--relay", &link, "--no-daemon", "--json"]).await);
            assert_eq!(retry["device_id"], id.device_id);
            assert_eq!(manager.roster()?.roster.version, before.roster.version);
            let renamed = cli(
                &home,
                &[
                    "up",
                    "--relay",
                    &link,
                    "--name",
                    "renamed",
                    "--no-daemon",
                    "--json",
                ],
            )
            .await;
            assert_eq!(renamed.status.code(), Some(125));
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&renamed.stderr)?["code"],
                "INVALID_REQUEST"
            );

            // Creating a new network archives a legacy identity and removes old grants.
            let legacy = root.path().join("legacy");
            let mut old = id.clone();
            old.network = None;
            config::write(&legacy.join(".xrun/identity.toml"), &old)?;
            let cfg = config::DaemonConfig {
                allow_all: true,
                allow_from: vec![id.device_id],
                ..Default::default()
            };
            config::write(&legacy.join(".xrun/daemon.toml"), &cfg)?;
            ok(cli(&legacy, &["up", "--relay", &link, "--no-daemon"]).await);
            let archived: Identity = config::read(&legacy.join(".xrun/identity.previous.toml"))?;
            assert_eq!(archived.key_pem, old.key_pem);
            let policy: config::DaemonConfig = config::read(&legacy.join(".xrun/daemon.toml"))?;
            assert!(!policy.allow_all && policy.allow_from.is_empty());
        }
        assert_eq!(std::fs::read(dir.join("root.key"))?, authority_key);
    }
    relay.stop().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconnect_after_identity_replacement_stops_instead_of_impersonating_the_new_device()
-> Result<()> {
    let mut lab = Lab::new().await?;
    config::write(
        &lab.target.join(".xrun/identity.toml"),
        &lab.source_identity,
    )?;
    lab.relay.stop().await?;
    let status =
        tokio::time::timeout(std::time::Duration::from_secs(10), lab.daemon.wait()).await??;
    assert_eq!(status.code(), Some(125));
    assert!(!config::instance_running(
        &lab.target.join(".xrun/daemon.lock")
    )?);
    assert!(
        xrun::testing::control::state(&lab.target.join(".xrun"))?
            .is_none_or(|state| !state.connected)
    );
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    Ok(())
}
