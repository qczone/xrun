mod common;
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use std::{process::Stdio, time::Duration};
use xrun::{
    config::{DaemonConfig, Identity},
    membership::RosterCache,
    store::TaskStore,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registration_permissions_migration_and_manager_offline_execution() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(25), async {
        let mut lab = Lab::new().await?;
        let ordinary = lab.root.path().join("ordinary");
        std::fs::create_dir_all(&ordinary)?;
        let invite = json(cli(&lab.source, &["invite", "--json"]).await);
        assert_eq!(invite["allow"], false);
        // The encrypted manager endpoint checks releases itself, even if an
        // untrusted relay accepts a mismatched component. The token survives.
        let network=&lab.source_identity.network.as_ref().unwrap().network_id;
        let roster=RosterCache::open(&lab.source.join(".xrun/roster.db"))?.load(network)?;
        let mut outer=xrun::net::websocket_at(&roster.roster.relay_addresses[0],
            &format!("/networks/{network}/pairing"),xrun::crypto::anonymous_tls_config(&roster.roster.relay_ca_pem)?).await?;
        assert!(matches!(xrun::net::receive(&mut outer).await?,xrun::relay::RelayMessage::Accepted{..}));
        assert!(matches!(xrun::net::receive(&mut outer).await?,xrun::relay::RelayMessage::Connected));
        let (mut peer,_)=xrun::secure::pairing_client(outer,&xrun::crypto::ca_spki_pin(&roster.ca_pem)?,&lab.source_identity.device_id).await?;
        xrun::net::send(&mut peer,&xrun::protocol::PairRequest {
            version:"incompatible".into(),token:invite["link"].as_str().unwrap().split_once('#').unwrap().1.into(),
            name:"ordinary1".into(),csr_base64:STANDARD.encode(xrun::crypto::new_device_request()?.1),
        }).await?;
        assert!(matches!(xrun::net::receive::<xrun::protocol::Data>(&mut peer).await?,xrun::protocol::Data::Error{code,..} if code=="VERSION_MISMATCH"));
        drop(peer);
        ok(cli(
            &ordinary,
            &[
                "join",
                invite["link"].as_str().unwrap(),
                "--name",
                "ordinary1",
                "--no-daemon",
            ],
        )
        .await);
        let mut daemon = command(&ordinary, &["daemon"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if json(cli(&lab.source, &["ordinary1", "info", "--json"]).await)["online"] == true
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        // Registration alone grants neither direction of execution.
        for (home, target) in [(&lab.source, "ordinary1"), (&ordinary, "source1")] {
            let output = cli(
                home,
                &[target, "--", env!("CARGO_BIN_EXE_xrun"), "--version"],
            )
            .await;
            assert_eq!(output.status.code(), Some(125));
            assert!(String::from_utf8_lossy(&output.stderr).contains("SOURCE_NOT_ALLOWED"));
        }
        let denied = cli(&ordinary, &["invite"]).await;
        assert!(String::from_utf8_lossy(&denied.stderr).contains("NOT_MANAGER"));
        // An old identity is archived; history/settings survive but old grants
        // and all-member trust never silently carry over into the new network.
        let migrated = lab.root.path().join("migrated");
        let dir = migrated.join(".xrun");
        std::fs::create_dir_all(&dir)?;
        let mut old = lab.target_identity.clone();
        old.network = None;
        xrun::config::write(&dir.join("identity.toml"), &old)?;
        let old_cfg = DaemonConfig {
            allow_from: vec!["dev_01234567890123456789012345678901".into()],
            deny_from: vec!["dev_11234567890123456789012345678901".into()],
            allow_all: true,
            remote_access_paused: true,
            pause_generation: 7,
            ..Default::default()
        };
        xrun::config::write(&dir.join("daemon.toml"), &old_cfg)?;
        let history = TaskStore::open(&dir.join("daemon.db"), true)?;
        history.audit(serde_json::json!({"op":"old-history","preserve":true}))?;
        std::fs::write(dir.join("daemon.initialized"), b"2\n")?;
        let allow = json(cli(&lab.source, &["invite", "--allow", "--json"]).await);
        ok(cli(
            &migrated,
            &[
                "join",
                allow["link"].as_str().unwrap(),
                "--name",
                "migrated1",
                "--no-daemon",
            ],
        )
        .await);
        let policy: DaemonConfig = xrun::config::read(&dir.join("daemon.toml"))?;
        assert_eq!(
            policy.allow_from,
            vec![lab.source_identity.device_id.clone()]
        );
        assert!(policy.deny_from.is_empty());
        assert!(!policy.allow_all);
        assert!(policy.remote_access_paused);
        assert_eq!(policy.pause_generation, 7);
        let saved: Identity = xrun::config::read(&dir.join("identity.previous.toml"))?;
        assert_eq!(saved.device_id, old.device_id);
        assert!(
            xrun::config::read::<Identity>(&dir.join("identity.toml"))?
                .network
                .is_some()
        );
        let db = rusqlite::Connection::open(dir.join("daemon.db"))?;
        let count: i64 = db.query_row(
            "SELECT COUNT(*) FROM audit WHERE json_extract(data,'$.op')='old-history'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1);
        // A missing local cache is an error; it cannot be silently recreated
        // from whatever older record an untrusted relay supplies.
        let cache_path = ordinary.join(".xrun/roster.db");
        let ordinary_id: Identity = xrun::config::read(&ordinary.join(".xrun/identity.toml"))?;
        let cache = RosterCache::open(&cache_path)?;
        let roster = cache.load(&ordinary_id.network.as_ref().unwrap().network_id)?;
        assert!(
            roster
                .roster
                .members
                .iter()
                .any(|m| m.device_id == lab.target_identity.device_id)
        );
        drop(cache);
        let hidden = ordinary.join(".xrun/roster.hidden");
        std::fs::rename(&cache_path, &hidden)?;
        let missing = cli(&ordinary, &["status", "--json"]).await;
        assert_eq!(missing.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&missing.stdout).contains("MEMBER_STATE_MISSING"));
        assert!(!cache_path.exists());
        std::fs::rename(&hidden, &cache_path)?;
        ok(cli(&lab.target, &["allow-from", "ordinary1"]).await);
        ok(cli(&ordinary, &["allow-from", "target1"]).await);
        lab.source_daemon.start_kill()?;
        lab.source_daemon.wait().await?;
        // Existing ordinary members keep executing through the relay without
        // the manager. Enrollment/renewal require its private signing authority.
        let version = ok(cli(
            &ordinary,
            &["target1", "--", env!("CARGO_BIN_EXE_xrun"), "--version"],
        )
        .await);
        assert!(version.contains("0.0.1-beta.1"));
        let version = ok(cli(
            &lab.target,
            &["ordinary1", "--", env!("CARGO_BIN_EXE_xrun"), "--version"],
        )
        .await);
        assert!(version.contains("0.0.1-beta.1"));
        daemon.start_kill()?;
        daemon.wait().await?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
