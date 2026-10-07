mod common;
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use common::*;
use std::time::Duration;
use xrun::testing::{
    config::{DaemonConfig, Identity},
    membership::RosterCache,
    store::TaskStore,
};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registration_permissions_migration_and_manager_offline_execution() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(25), async {
        let mut lab = Lab::new().await?;
        let invalid = lab.root.path().join("invalid-route");
        std::fs::create_dir_all(&invalid)?;
        let link = xrun::testing::relay::deployment_link(&lab.relay.config)?;
        let wrong = format!("{}#{}", link.split_once('#').unwrap().0, "a".repeat(26));
        let denied = cli(&invalid, &["up", "--relay", &wrong, "--no-daemon"]).await;
        assert_eq!(denied.status.code(), Some(125));
        assert!(!invalid.join(".xrun/identity.toml").exists());
        assert!(!invalid.join(".xrun/manager/manager.db").exists());
        let ordinary = lab.root.path().join("ordinary");
        std::fs::create_dir_all(&ordinary)?;
        let invite = json(cli(&lab.source, &["invite", "--json"]).await);
        assert_eq!(invite["allow"], false);
        // The encrypted manager rejects disjoint protocols before consuming
        // an invitation, even if a relay accepts the outer connection.
        let network=&lab.source_identity.network.as_ref().unwrap().network_id;
        let roster=RosterCache::open(&lab.source.join(".xrun/roster.db"))?.load(network)?;
        let mut outer=common::relay_socket(None,&roster,
            &format!("/networks/{network}/connect/{}", lab.source_identity.device_id)).await?;
        assert!(matches!(xrun::testing::net::receive(&mut outer).await?,xrun::protocol::RelayMessage::Connected { .. }));
        let (mut peer,_)=xrun::testing::secure::pairing_client(outer,&xrun::testing::crypto::ca_spki_pin(&roster.ca_pem)?,&lab.source_identity.device_id).await?;
        xrun::testing::net::send(&mut peer,&xrun::protocol::PairRequest {
            version:"incompatible".into(),token:invite["link"].as_str().unwrap().split_once('#').unwrap().1.into(),
            protocol: xrun::protocol::ProtocolRange { min: xrun::protocol::PROTOCOL + 1, max: xrun::protocol::PROTOCOL + 1 },
            name:"ordinary1".into(),csr_base64:STANDARD.encode(xrun::testing::crypto::new_device_request()?.1),
        }).await?;
        assert!(matches!(xrun::testing::net::receive::<xrun::protocol::Data>(&mut peer).await?,xrun::protocol::Data::Error{code,..} if code=="VERSION_MISMATCH"));
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
        let mut daemon = logged(&ordinary, &["daemon"],"ordinary")?
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
        // Knowing the relay route without a member certificate only permits
        // pairing with the manager, never execution or task-history access.
        let path = format!("/networks/{network}/connect/{}", lab.target_identity.device_id);
        let mut outer = common::relay_socket(None, &roster, &path).await?;
        assert!(matches!(xrun::testing::net::receive(&mut outer).await?, xrun::protocol::RelayMessage::Error { code, .. } if code == "UNAUTHENTICATED"));
        let manager = &lab.source_identity.device_id;
        let mut outer = common::relay_socket(None, &roster, &format!("/networks/{network}/connect/{manager}")).await?;
        assert!(matches!(xrun::testing::net::receive(&mut outer).await?, xrun::protocol::RelayMessage::Connected { .. }));
        let (mut anonymous, _) = xrun::testing::secure::pairing_client(outer, &xrun::testing::crypto::ca_spki_pin(&roster.ca_pem)?, manager).await?;
        // The manager may reject and close before reading the request, so a
        // failed write is a refusal too.
        let answer = async {
            xrun::testing::net::send(&mut anonymous, &xrun::protocol::Data::Request { request: xrun::protocol::Request::Jobs { id: None, running: false, request_id: None, limit: 10, offset: 0 } }).await?;
            xrun::testing::net::receive::<xrun::protocol::Data>(&mut anonymous).await
        }.await;
        assert!(matches!(answer, Err(_) | Ok(xrun::protocol::Data::Error { .. })));
        // Registration alone grants neither direction of execution.
        for (home, target) in [(&lab.source, "ordinary1"), (&ordinary, "source1")] {
            let output = cli(
                home,
                &[target, "--", &binary().to_string_lossy(), "--version"],
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
        xrun::testing::config::write(&dir.join("identity.toml"), &old)?;
        let old_cfg = DaemonConfig {
            allow_from: vec!["dev_01234567890123456789012345678901".into()],
            deny_from: vec!["dev_11234567890123456789012345678901".into()],
            allow_all: true,
            remote_access_paused: true,
            pause_generation: 7,
            ..Default::default()
        };
        xrun::testing::config::write(&dir.join("daemon.toml"), &old_cfg)?;
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
        let policy: DaemonConfig = xrun::testing::config::read(&dir.join("daemon.toml"))?;
        assert_eq!(
            policy.allow_from,
            vec![lab.source_identity.device_id.clone()]
        );
        assert!(policy.deny_from.is_empty());
        assert!(!policy.allow_all);
        assert!(policy.remote_access_paused);
        assert_eq!(policy.pause_generation, 7);
        let saved: Identity = xrun::testing::config::read(&dir.join("identity.previous.toml"))?;
        assert_eq!(saved.device_id, old.device_id);
        assert!(
            xrun::testing::config::read::<Identity>(&dir.join("identity.toml"))?
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
        let ordinary_id: Identity = xrun::testing::config::read(&ordinary.join(".xrun/identity.toml"))?;
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
        // Release SQLite handles before moving the cache, including on Windows.
        stop_daemon(&ordinary, &mut daemon).await?;
        let hidden = ordinary.join(".xrun/roster.hidden");
        std::fs::rename(&cache_path, &hidden)?;
        let missing = cli(&ordinary, &["status", "--json"]).await;
        assert_eq!(missing.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&missing.stdout).contains("MEMBER_STATE_MISSING"));
        assert!(!cache_path.exists());
        std::fs::rename(&hidden, &cache_path)?;
        daemon = logged(&ordinary, &["daemon"], "ordinary")?.spawn()?;
        online(&lab.source, "ordinary1").await?;
        ok(cli(&lab.target, &["allow-from", "ordinary1"]).await);
        ok(cli(&ordinary, &["allow-from", "target1"]).await);
        lab.source_daemon.start_kill()?;
        lab.source_daemon.wait().await?;
        // Existing ordinary members keep executing through the relay without
        // the manager. Enrollment/renewal require its private signing authority.
        let version = ok(cli(
            &ordinary,
            &["target1", "--", &binary().to_string_lossy(), "--version"],
        )
        .await);
        assert!(version.contains(xrun::protocol::VERSION));
        let version = ok(cli(
            &lab.target,
            &["ordinary1", "--", &binary().to_string_lossy(), "--version"],
        )
        .await);
        assert!(version.contains(xrun::protocol::VERSION));
        daemon.start_kill()?;
        daemon.wait().await?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn device_operations_connect_directly_while_info_still_queries_live_state() -> Result<()> {
    use anyhow::Context;
    use xrun::testing::{
        crypto, membership::ReceiptAck, net, network::PeerState, protocol::RelayMessage,
        protocol::*, secure,
    };
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut lab = Lab::new().await?;
        let device: Device =
            serde_json::from_value(json(cli(&lab.source, &["target1", "info", "--json"]).await))?;
        let observer = lab.root.path().join("observer");
        std::fs::create_dir_all(&observer)?;
        let invite = json(cli(&lab.source, &["invite", "--json"]).await);
        ok(cli(
            &observer,
            &[
                "join",
                invite["link"].as_str().unwrap(),
                "--name",
                "observer1",
                "--no-daemon",
            ],
        )
        .await);
        let observer_id: Identity =
            xrun::testing::config::read(&observer.join(".xrun/identity.toml"))?;
        stop_daemon(&lab.source, &mut lab.source_daemon).await?;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let cache = RosterCache::open(&lab.target.join(".xrun/roster.db"))?;
        let network = &lab.target_identity.network.as_ref().unwrap().network_id;
        let roster = cache.load(network)?;
        let mut control = relay_socket(
            Some(&lab.target_identity),
            &roster,
            &format!("/networks/{network}/control"),
        )
        .await?;
        let RelayMessage::HelloAck { generation } = net::receive(&mut control).await? else {
            anyhow::bail!("control acknowledgement")
        };
        let observer_roster =
            RosterCache::open(&observer.join(".xrun/roster.db"))?.load(network)?;
        let mut observer_control = relay_socket(
            Some(&observer_id),
            &observer_roster,
            &format!("/networks/{network}/control"),
        )
        .await?;
        assert!(matches!(
            net::receive(&mut observer_control).await?,
            RelayMessage::HelloAck { .. }
        ));
        let peer = async {
            for state_query in [false, true] {
                let RelayMessage::Incoming { session_id } = net::receive(&mut control).await?
                else {
                    anyhow::bail!("incoming session")
                };
                let mut outer = net::websocket_at(
                    &roster.roster.relay_addresses[0],
                    &format!(
                        "/networks/{network}/attach/{}/{generation}/{session_id}",
                        lab.target_identity.device_id
                    ),
                    crypto::relay_tls_config(&roster.roster.relay_ca_pem)?,
                )
                .await?;
                assert!(matches!(
                    net::receive(&mut outer).await?,
                    RelayMessage::Connected { .. }
                ));
                let (mut ws, certificate) = secure::server(outer, &lab.target_identity).await?;
                let (_, source, _) = secure::exchange_server(
                    &mut ws,
                    &cache,
                    network,
                    &certificate.context("peer certificate")?,
                )
                .await?;
                assert_eq!(source, lab.source_identity.device_id);
                let purpose: secure::Purpose = net::receive(&mut ws).await?;
                assert_eq!(
                    matches!(purpose, secure::Purpose::State),
                    state_query,
                    "ordinary operations must not open a preliminary state session"
                );
                if state_query {
                    net::send(
                        &mut ws,
                        &PeerState {
                            device: device.clone(),
                            ack: ReceiptAck::create(&lab.target_identity, &roster)?,
                        },
                    )
                    .await?;
                } else {
                    net::send(
                        &mut ws,
                        &Data::Ready {
                            version: VERSION.into(),
                            protocol: xrun::protocol::ProtocolRange::CURRENT,
                            selected_protocol: xrun::protocol::PROTOCOL,
                            device_id: lab.target_identity.device_id.clone(),
                            db_id: "test-database".into(),
                            default_cwd: lab.target.to_string_lossy().into(),
                        },
                    )
                    .await?;
                    assert!(matches!(
                        net::receive(&mut ws).await?,
                        Data::Request {
                            request: Request::Jobs { .. }
                        }
                    ));
                    net::send(
                        &mut ws,
                        &Data::Jobs {
                            jobs: vec![],
                            next_offset: None,
                        },
                    )
                    .await?;
                }
                net::close(&mut ws).await;
            }
            Ok::<_, anyhow::Error>(())
        };
        let commands = async {
            let status = json(cli(&lab.source, &["status", "--json"]).await);
            let connected = status["devices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["device_id"] == lab.target_identity.device_id)
                .unwrap();
            assert_eq!(connected["online"], true);
            assert!(connected["hostname"].is_null());
            assert!(connected["version"].is_null());
            let jobs = json(cli(&lab.source, &["target1", "jobs", "--json"]).await);
            assert_eq!(jobs, serde_json::json!([]));
            let info = json(cli(&lab.source, &["target1", "info", "--json"]).await);
            assert_eq!(info["online"], true);
            assert_eq!(info["device_id"], lab.target_identity.device_id);
            assert_eq!(info["hostname"], serde_json::to_value(&device.hostname)?);
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(peer, commands)?;
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                net::receive::<RelayMessage>(&mut observer_control)
            )
            .await
            .is_err(),
            "status and target info must not contact other peers"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_filters_known_revocations_without_claiming_roster_synchronization() -> Result<()> {
    use xrun::testing::{net, protocol::RelayMessage};
    tokio::time::timeout(Duration::from_secs(25), async {
        let mut lab = Lab::new().await?;
        let observer = lab.root.path().join("observer");
        std::fs::create_dir_all(&observer)?;
        let invite = json(cli(&lab.source, &["invite", "--json"]).await);
        ok(cli(
            &observer,
            &[
                "join",
                invite["link"].as_str().unwrap(),
                "--name",
                "observer1",
                "--no-daemon",
            ],
        )
        .await);
        let network = &lab.target_identity.network.as_ref().unwrap().network_id;
        let cache = RosterCache::open(&observer.join(".xrun/roster.db"))?;
        let old = cache.load(network)?;
        stop_daemon(&lab.target, &mut lab.daemon).await?;
        let revoked = json(cli(&lab.source, &["revoke", "target1", "--json"]).await);
        assert!(
            revoked["undelivered"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d == &old.member("observer1").unwrap().device_id)
        );
        // The relay has no revocation list. A certificate can still bind its own
        // route, but a client with the signed revocation must not display it online.
        let roster = RosterCache::open(&lab.target.join(".xrun/roster.db"))?.load(network)?;
        let mut control = relay_socket(
            Some(&lab.target_identity),
            &roster,
            &format!("/networks/{network}/control"),
        )
        .await?;
        assert!(matches!(
            net::receive(&mut control).await?,
            RelayMessage::HelloAck { .. }
        ));
        let target = |status: serde_json::Value| {
            status["devices"]
                .as_array()
                .unwrap()
                .iter()
                .find(|d| d["device_id"] == lab.target_identity.device_id)
                .unwrap()
                .clone()
        };
        let known = target(json(cli(&lab.source, &["status", "--json"]).await));
        assert_eq!(known["revoked"], true);
        assert_eq!(known["online"], false);
        let stale = target(json(cli(&observer, &["status", "--json"]).await));
        assert_eq!(stale["revoked"], false);
        assert_eq!(stale["online"], true);
        assert_eq!(cache.load(network)?.roster.version, old.roster.version);
        // End-to-end info still synchronizes signed membership, independently of
        // the lightweight list and without probing the revoked device.
        let info = json(cli(&observer, &["source1", "info", "--json"]).await);
        assert_eq!(info["online"], true);
        assert!(cache.load(network)?.member("target1")?.revoked);
        assert_eq!(
            target(json(cli(&observer, &["status", "--json"]).await))["online"],
            false
        );
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
