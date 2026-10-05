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
async fn relay_cannot_substitute_target_or_pairing_manager_and_acknowledgements_cannot_be_forged()
-> Result<()> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir()?;
        let (manager, manager_id, source, target) = identities(dir.path())?;
        let roster = manager.roster()?;
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
        assert!(ack.verify(&manager.revoke("source1")?).is_err());
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

#[tokio::test]
async fn final_response_survives_a_relay_that_buffers_until_after_close_is_sent() -> Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::sync::{Notify, mpsc, oneshot};
    tokio::time::timeout(Duration::from_secs(10), async {
        let dir = tempfile::tempdir()?;
        let (_, _, source, target) = identities(dir.path())?;
        let (wire_a, relay_a) = wire().await;
        let (wire_b, relay_b) = wire().await;
        let delaying = Arc::new(AtomicBool::new(false));
        let release = Arc::new(Notify::new());
        let blocked = Arc::new(Notify::new());
        let pump = {
            let delaying = delaying.clone();
            let release = release.clone();
            let blocked = blocked.clone();
            tokio::spawn(async move {
                let (mut a_tx, mut a_rx) = relay_a.split();
                let (mut b_tx, mut b_rx) = relay_b.split();
                let (queue, mut pending) = mpsc::channel(64);
                let receive = async {
                    while let Some(message) = b_rx.next().await {
                        queue.send(message?).await?;
                    }
                    Ok::<_, anyhow::Error>(())
                };
                let forward = async {
                    let mut released = false;
                    while let Some(message) = pending.recv().await {
                        if delaying.load(Ordering::SeqCst) && !released {
                            blocked.notify_one();
                            release.notified().await;
                            released = true;
                        }
                        a_tx.send(message).await?;
                    }
                    Ok::<_, anyhow::Error>(())
                };
                let reverse = async {
                    while let Some(message) = a_rx.next().await {
                        b_tx.send(message?).await?;
                    }
                    Ok::<_, anyhow::Error>(())
                };
                // As on the hosted relay, disconnecting either endpoint tears
                // down both directions, including any undelivered ciphertext.
                tokio::select! { _ = receive => {}, _ = forward => {}, _ = reverse => {} }
            })
        };
        let (ready, ready_rx) = oneshot::channel();
        let (sent, sent_rx) = oneshot::channel();
        let (finished, mut finished_rx) = oneshot::channel();
        let content = vec![71; 128 * 1024];
        let server = async {
            let (mut ws, _) = secure::server(wire_b, &target).await?;
            ready_rx.await?;
            delaying.store(true, Ordering::SeqCst);
            net::send_bytes(&mut ws, &content).await?;
            let _ = sent.send(());
            net::close(&mut ws).await;
            let _ = finished.send(());
            Ok::<_, anyhow::Error>(())
        };
        let client = async {
            let (mut ws, _) = secure::client(wire_a, &source, &target.device_id).await?;
            let _ = ready.send(());
            blocked.notified().await;
            sent_rx.await?;
            // All writes have completed, but the relay still holds the body.
            assert!(
                tokio::time::timeout(Duration::from_millis(50), &mut finished_rx)
                    .await
                    .is_err()
            );
            release.notify_one();
            let received =
                net::receive_bytes(&mut ws, content.len() as u64, &sha256(&content), MAX_FILE)
                    .await?;
            assert_eq!(received, content);
            net::close(&mut ws).await;
            Ok::<_, anyhow::Error>(())
        };
        let result = tokio::try_join!(server, client);
        pump.abort();
        result?;
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
