use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};
use xrun::{
    config::{Identity, ServerConfig},
    crypto,
    membership::*,
    net::{self, Io, Ws},
    protocol::*,
    relay::*,
    secure,
};

fn identities(dir: &std::path::Path) -> Result<(Manager, Identity, Identity, Identity)> {
    let transport = crypto::load_or_create_server(&ServerConfig {
        port: 9528,
        addresses: vec!["127.0.0.1:9528".into()],
        manual: true,
        no_detect: true,
        data_dir: dir.join("relay"),
    })?;
    let (manager, member, key_pem, cert_pem) = Manager::create(
        &dir.join("manager"),
        "manager1",
        vec!["https://127.0.0.1:9528".into()],
        transport.ca_pem,
    )?;
    let root = manager.roster()?.ca_pem;
    let manager_id = Identity {
        network: None,
        device_id: member.device_id,
        name: member.name,
        addresses: vec![],
        ca_pem: root.clone(),
        cert_pem,
        key_pem,
        registration: Registration {
            inviter_id: None,
            allow_inviter: false,
        },
    };
    let mut members = vec![];
    for name in ["source1", "target1"] {
        let (key_pem, csr) = crypto::new_device_request()?;
        let pair = manager.pair(&manager.invite(false)?, name, &csr)?;
        members.push(Identity {
            network: None,
            device_id: pair.member.device_id,
            name: name.into(),
            addresses: vec![],
            ca_pem: root.clone(),
            cert_pem: pair.cert_pem,
            key_pem,
            registration: Registration {
                inviter_id: Some(manager_id.device_id.clone()),
                allow_inviter: false,
            },
        });
    }
    let target = members.pop().unwrap();
    let source = members.pop().unwrap();
    Ok((manager, manager_id, source, target))
}
async fn wire() -> (Ws, Ws) {
    let (a, b) = tokio::io::duplex(256 * 1024);
    let a = WebSocketStream::from_raw_socket(Box::new(a) as Io, Role::Client, None).await;
    let b = WebSocketStream::from_raw_socket(Box::new(b) as Io, Role::Server, None).await;
    (a, b)
}
async fn intercepted() -> (Ws, Ws, tokio::task::JoinHandle<()>, Arc<Mutex<Vec<u8>>>) {
    let (source, relay_source) = wire().await;
    let (target, relay_target) = wire().await;
    let bytes = Arc::new(Mutex::new(vec![]));
    let recorded = bytes.clone();
    let pump = tokio::spawn(async move {
        let (a_tx, a_rx) = relay_source.split();
        let (b_tx, b_rx) = relay_target.split();
        let copy = async |mut reader: futures_util::stream::SplitStream<Ws>,
                          mut writer: futures_util::stream::SplitSink<Ws, Message>|
               -> Result<()> {
            while let Some(m) = reader.next().await {
                let m = m?;
                if let Message::Binary(data) = &m {
                    recorded.lock().unwrap().extend_from_slice(data);
                }
                writer.send(m).await?;
            }
            Ok(())
        };
        tokio::select! {_=copy(a_rx,b_tx)=>{},_=copy(b_rx,a_tx)=>{}}
    });
    (source, target, pump, bytes)
}

#[tokio::test]
async fn manager_offline_members_use_mutual_tls_and_relay_only_sees_ciphertext() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir()?;
        let (manager, _, source, target) = identities(dir.path())?;
        let roster = manager.roster()?;
        let network = roster.roster.network_id.clone();
        drop(manager);
        let a = RosterCache::open(&dir.path().join("a/cache.db"))?;
        let b = RosterCache::open(&dir.path().join("b/cache.db"))?;
        a.observe(&network, &roster)?;
        b.observe(&network, &roster)?;
        let (wire_a, wire_b, pump, bytes) = intercepted().await;
        let secret = "secret-command-file-env-output-347ad64f";
        let server = async {
            let (mut ws, cert) = secure::server(wire_b, &target).await?;
            let (_, actual) = secure::exchange_server(
                &mut ws,
                &b,
                &network,
                &cert.context("missing peer certificate")?,
            )
            .await?;
            assert_eq!(actual, source.device_id);
            assert_eq!(net::receive::<String>(&mut ws).await?, secret);
            net::send(&mut ws, &secret).await?;
            let _: String = net::receive(&mut ws).await?;
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let (mut ws, cert) = secure::client(wire_a, &source, &target.device_id).await?;
            secure::exchange_client(&mut ws, &a, &network, &cert, &target.device_id).await?;
            net::send(&mut ws, &secret).await?;
            assert_eq!(net::receive::<String>(&mut ws).await?, secret);
            net::send(&mut ws, &"received").await?;
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(server, client)?;
        pump.abort();
        let bytes = bytes.lock().unwrap();
        assert!(!bytes.is_empty());
        assert!(!bytes.windows(secret.len()).any(|w| w == secret.as_bytes()));
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn relay_cannot_substitute_target_or_pairing_manager_and_proofs_cannot_replay() -> Result<()>
{
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir()?;
        let (manager, manager_id, source, target) = identities(dir.path())?;
        let roster = manager.roster()?;
        let network = &roster.roster.network_id;
        let proof = Proof::create(&source, network, "/correct", "fresh")?;
        proof.verify(&roster, "/correct", "fresh")?;
        assert!(proof.verify(&roster, "/correct", "replayed").is_err());
        assert!(proof.verify(&roster, "/wrong", "fresh").is_err());
        let mut impersonated = proof.clone();
        impersonated.device_id = target.device_id.clone();
        assert!(impersonated.verify(&roster, "/correct", "fresh").is_err());
        let ack = ReceiptAck::create(&source, &roster)?;
        ack.verify(&roster)?;
        let mut fake = ack.clone();
        fake.device_id = target.device_id.clone();
        assert!(fake.verify(&roster).is_err());
        for pairing in [false, true] {
            let (wire_a, wire_b, pump, _) = intercepted().await;
            let server = secure::server(wire_b, &target);
            let client = async {
                if pairing {
                    assert!(
                        secure::pairing_client(
                            wire_a,
                            &crypto::ca_spki_pin(&roster.ca_pem)?,
                            &manager_id.device_id
                        )
                        .await
                        .is_err()
                    );
                } else {
                    assert!(
                        secure::client(wire_a, &source, &manager_id.device_id)
                            .await
                            .is_err()
                    );
                }
                Ok::<_, anyhow::Error>(())
            };
            let (_, result) = tokio::join!(server, client);
            result?;
            pump.abort();
        }
        assert!(
            proof
                .verify(&manager.revoke("source1")?, "/correct", "fresh")
                .is_err()
        );
        Ok::<_, anyhow::Error>(())
    })
    .await??;
    Ok(())
}

#[tokio::test]
async fn final_large_response_survives_backpressure_and_immediate_close() -> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir()?;
        let (manager, _, source, target) = identities(dir.path())?;
        let roster = manager.roster()?;
        let network = &roster.roster.network_id;
        let a = RosterCache::open(&dir.path().join("a/cache.db"))?;
        let b = RosterCache::open(&dir.path().join("b/cache.db"))?;
        a.observe(network, &roster)?;
        b.observe(network, &roster)?;
        let (wire_a, wire_b, pump, _) = intercepted().await;
        let content: Vec<u8> = (0..1_500_000).map(|i| (i % 256) as u8).collect();
        let server = async {
            let (mut ws, cert) = secure::server(wire_b, &target).await?;
            secure::exchange_server(&mut ws, &b, network, &cert.context("client certificate")?)
                .await?;
            net::send_bytes(&mut ws, &content).await?;
            ws.close(None).await?;
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let (mut ws, cert) = secure::client(wire_a, &source, &target.device_id).await?;
            secure::exchange_client(&mut ws, &a, network, &cert, &target.device_id).await?;
            tokio::time::sleep(Duration::from_millis(100)).await;
            let received =
                net::receive_bytes(&mut ws, content.len() as u64, &sha256(&content), MAX_FILE)
                    .await?;
            assert_eq!(received.len(), content.len());
            Ok::<_, anyhow::Error>(())
        };
        tokio::try_join!(server, client)?;
        pump.abort();
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
