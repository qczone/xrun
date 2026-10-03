mod common;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use xrun::{
    config::{Identity, NetworkIdentity, ServerConfig},
    crypto,
    membership::{Manager, SignedRoster},
    net,
    protocol::{Registration, VERSION},
    relay::{Proof, RelayMessage},
};

struct Relay(tokio::task::JoinHandle<Result<()>>);
impl Drop for Relay {
    fn drop(&mut self) {
        self.0.abort();
    }
}
fn identity(manager: &Manager, name: &str) -> Result<Identity> {
    let token = manager.invite(false)?;
    let (key, csr) = crypto::new_device_request()?;
    let pair = manager.pair(&token, name, &csr)?;
    Ok(Identity {
        device_id: pair.member.device_id,
        name: pair.member.name,
        key_pem: key,
        cert_pem: pair.cert_pem,
        addresses: pair.roster.roster.relay_addresses.clone(),
        ca_pem: pair.roster.ca_pem,
        registration: Registration {
            inviter_id: Some(pair.roster.roster.manager_id.clone()),
            allow_inviter: false,
        },
        network: Some(NetworkIdentity {
            network_id: pair.roster.roster.network_id,
            manager_id: pair.roster.roster.manager_id,
        }),
    })
}
async fn control(id: &Identity, roster: &SignedRoster) -> Result<net::Ws> {
    let mut ws = common::relay_socket(
        id,
        roster,
        &format!("/networks/{}/control", roster.roster.network_id),
    )
    .await?;
    net::send(
        &mut ws,
        &RelayMessage::Hello {
            version: VERSION.into(),
            os: "test".into(),
            arch: "test".into(),
            hostname: None,
            execution_user: None,
            default_cwd: None,
        },
    )
    .await?;
    assert!(matches!(
        net::receive(&mut ws).await?,
        RelayMessage::HelloAck
    ));
    Ok(ws)
}
async fn source(id: &Identity, target: &Identity, roster: &SignedRoster) -> Result<net::Ws> {
    common::relay_socket(
        id,
        roster,
        &format!(
            "/networks/{}/connect/{}",
            roster.roster.network_id, target.device_id
        ),
    )
    .await
}
async fn incoming(ws: &mut net::Ws) -> Result<String> {
    let RelayMessage::Incoming { session_id, .. } = net::receive(ws).await? else {
        anyhow::bail!("missing incoming session")
    };
    Ok(session_id)
}
async fn attach(id: &Identity, roster: &SignedRoster, sid: &str) -> Result<net::Ws> {
    let mut ws = common::relay_socket(
        id,
        roster,
        &format!("/networks/{}/attach/{sid}", roster.roster.network_id),
    )
    .await?;
    match net::receive(&mut ws).await? {
        RelayMessage::Connected => Ok(ws),
        RelayMessage::Error { code, message } => anyhow::bail!("{code}: {message}"),
        _ => anyhow::bail!("invalid attach result"),
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_authentication_binding_and_limits() -> Result<()> {
    common::library_logs()?;
    tokio::time::timeout(Duration::from_secs(25), async {
        let temp = tempfile::tempdir()?;
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let cfg = ServerConfig {
            port,
            addresses: vec![format!("127.0.0.1:{port}")],
            manual: true,
            no_detect: true,
            data_dir: temp.path().join("relay"),
        };
        let link = xrun::relay::enrollment(&cfg)?;
        let keys = crypto::load_or_create_server(&cfg)?;
        let (manager, _, _, _) = Manager::create(
            &temp.path().join("manager"),
            "manager1",
            cfg.urls(),
            keys.ca_pem.clone(),
        )?;
        let a = identity(&manager, "target1")?;
        let b = identity(&manager, "source1")?;
        let c = identity(&manager, "other1")?;
        let roster = manager.roster()?;
        let server = Relay(tokio::spawn(xrun::relay::run(cfg.clone())));
        tokio::time::timeout(Duration::from_secs(5), async {
            while tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err()
            {
                assert!(!server.0.is_finished());
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?;
        assert!(
            crypto::discover_ca(&cfg.urls()[0], &"a".repeat(52))
                .await
                .is_err()
        );
        let client = crypto::http_client(&keys.ca_pem, None)?;
        let publish_path = format!(
            "{}/networks/{}/roster",
            cfg.urls()[0],
            roster.roster.network_id
        );
        // Unknown networks require local operator admission. The relay never
        // receives the manager's member invitation token or network private key.
        let response = client
            .post(&publish_path)
            .header("x-xrun-version", VERSION)
            .json(&roster)
            .send()
            .await?;
        assert!(!response.status().is_success());
        let response = client
            .post(&publish_path)
            .header("x-xrun-version", VERSION)
            .header("x-xrun-enrollment", link.split_once('#').unwrap().1)
            .json(&roster)
            .send()
            .await?;
        assert!(response.status().is_success(), "{}", response.text().await?);
        // Proofs are bound to this connection's random challenge and exact path.
        let path = format!("/networks/{}/status", roster.roster.network_id);
        let tls = crypto::anonymous_tls_config(&keys.ca_pem)?;
        let mut first = net::websocket_at(&cfg.urls()[0], &path, tls.clone()).await?;
        let RelayMessage::Challenge { nonce } = net::receive(&mut first).await? else {
            unreachable!()
        };
        let proof = Proof::create(&b, &roster.roster.network_id, &path, &nonce)?;
        let mut second = net::websocket_at(&cfg.urls()[0], &path, tls.clone()).await?;
        assert!(matches!(
            net::receive(&mut second).await?,
            RelayMessage::Challenge { .. }
        ));
        net::send(
            &mut second,
            &RelayMessage::Authenticate {
                proof: proof.clone(),
            },
        )
        .await?;
        assert!(matches!(
            net::receive(&mut second).await?,
            RelayMessage::Error { .. }
        ));
        net::send(&mut first, &RelayMessage::Authenticate { proof }).await?;
        assert!(matches!(
            net::receive(&mut first).await?,
            RelayMessage::Accepted { .. }
        ));
        assert!(matches!(
            net::receive(&mut first).await?,
            RelayMessage::Status { .. }
        ));
        drop(first);
        drop(second);
        let mut ctl = control(&a, &roster).await?;
        let mut cli = source(&b, &a, &roster).await?;
        let sid = incoming(&mut ctl).await?;
        assert!(attach(&c, &roster, &sid).await.is_err());
        let mut data = attach(&a, &roster, &sid).await?;
        assert!(matches!(
            net::receive(&mut cli).await?,
            RelayMessage::Connected
        ));
        assert!(attach(&a, &roster, &sid).await.is_err());
        let ciphertext = vec![23, 3, 3, 0, 6, 0, 255, 128, 17, 1, 2];
        data.send(Message::Binary(ciphertext.clone().into()))
            .await?;
        loop {
            let message = tokio::time::timeout(Duration::from_secs(2), cli.next())
                .await?
                .context("source closed")??;
            if let Message::Binary(bytes) = message {
                assert_eq!(bytes.as_ref(), ciphertext.as_slice());
                break;
            }
        }
        // Replacing control invalidates the previous generation's tunnels.
        let mut new_control = control(&a, &roster).await?;
        let closed = tokio::time::timeout(Duration::from_secs(2), cli.next()).await?;
        assert!(closed.is_none() || matches!(closed, Some(Ok(Message::Close(_))) | Some(Err(_))));
        drop(data);
        drop(cli);
        drop(ctl);
        let mut early = source(&b, &a, &roster).await?;
        let sid = incoming(&mut new_control).await?;
        early.close(None).await?;
        drop(early);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(attach(&a, &roster, &sid).await.is_err());
        let mut pending = vec![];
        for _ in 0..16 {
            pending.push(source(&b, &a, &roster).await?);
            incoming(&mut new_control).await?;
        }
        let mut limit = source(&b, &a, &roster).await?;
        let RelayMessage::Error { code, .. } = net::receive(&mut limit).await? else {
            anyhow::bail!("missing limit error")
        };
        assert_eq!(code, "SESSION_LIMIT");
        drop(pending);
        drop(limit);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let revoked = manager.revoke(&c.device_id)?;
        assert!(
            client
                .post(&publish_path)
                .header("x-xrun-version", VERSION)
                .json(&revoked)
                .send()
                .await?
                .status()
                .is_success()
        );
        assert!(
            common::relay_socket(&c, &revoked, &path)
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("DEVICE_REVOKED")
        );
        // A signed older roster cannot roll back the relay's persisted cache.
        let old = client
            .post(&publish_path)
            .header("x-xrun-version", VERSION)
            .json(&roster)
            .send()
            .await?;
        assert!(!old.status().is_success());
        assert!(old.text().await?.contains("ROSTER_ROLLBACK"));
        let mut limited = false;
        for _ in 0..61 {
            let response = client
                .post(&publish_path)
                .header("x-xrun-version", VERSION)
                .json(&revoked)
                .send()
                .await?;
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                limited = true;
                break;
            }
        }
        assert!(limited, "publication rate limit did not apply");
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
