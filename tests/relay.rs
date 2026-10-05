mod common;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use xrun::{
    config::{Identity, ServerConfig},
    crypto,
    membership::Manager,
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
struct Network {
    id: String,
    manager: Manager,
    manager_identity: Identity,
    source: Identity,
    target: Identity,
}
fn identity(
    device_id: String,
    name: &str,
    root: &str,
    cert_pem: String,
    key_pem: String,
) -> Identity {
    Identity {
        network: None,
        device_id,
        name: name.into(),
        addresses: vec![],
        ca_pem: root.into(),
        cert_pem,
        key_pem,
        registration: Registration {
            inviter_id: None,
            allow_inviter: false,
        },
    }
}
fn network(dir: &std::path::Path, cfg: &ServerConfig) -> Result<Network> {
    let (manager, member, key_pem, cert_pem) = Manager::create(
        dir,
        "manager1",
        xrun::relay::addresses(cfg)?,
        crypto::load_or_create_server(cfg)?.ca_pem,
    )?;
    let root = manager.roster()?.ca_pem;
    let manager_identity = identity(member.device_id, "manager1", &root, cert_pem, key_pem);
    let mut members = vec![];
    for name in ["source1", "target1"] {
        let (key_pem, csr) = crypto::new_device_request()?;
        let pair = manager.pair(&manager.invite(false)?, name, &csr)?;
        members.push(identity(
            pair.member.device_id,
            name,
            &root,
            pair.cert_pem,
            key_pem,
        ));
    }
    let target = members.pop().unwrap();
    let source = members.pop().unwrap();
    Ok(Network {
        id: manager.roster()?.roster.network_id,
        manager,
        manager_identity,
        source,
        target,
    })
}
async fn socket(cfg: &ServerConfig, path: &str) -> Result<net::Ws> {
    let keys = crypto::load_or_create_server(cfg)?;
    net::websocket_at(
        &xrun::relay::addresses(cfg)?[0],
        path,
        crypto::anonymous_tls_config(&keys.ca_pem)?,
    )
    .await
}
async fn authed(
    cfg: &ServerConfig,
    network: &str,
    path: &str,
    id: Option<&Identity>,
    manager: Option<&Manager>,
) -> Result<net::Ws> {
    let mut ws = socket(cfg, path).await?;
    xrun::network::authenticate(&mut ws, id, network, path, manager).await?;
    Ok(ws)
}
async fn forged(
    cfg: &ServerConfig,
    path: &str,
    make: impl FnOnce(&str) -> Result<Proof>,
) -> Result<net::Ws> {
    let mut ws = socket(cfg, path).await?;
    let RelayMessage::Challenge { nonce } = net::receive(&mut ws).await? else {
        anyhow::bail!("missing relay challenge")
    };
    net::send(
        &mut ws,
        &RelayMessage::Authenticate {
            proof: Some(make(&nonce)?),
        },
    )
    .await?;
    Ok(ws)
}
async fn refused(ws: &mut net::Ws, expected: &str) -> Result<()> {
    match net::receive(ws).await? {
        RelayMessage::Error { code, .. } if code == expected => Ok(()),
        other => anyhow::bail!("expected {expected}, received {other:?}"),
    }
}
async fn control(
    cfg: &ServerConfig,
    network: &str,
    id: &Identity,
    manager: Option<&Manager>,
) -> Result<(net::Ws, String)> {
    let path = format!("/networks/{network}/control");
    let mut ws = authed(cfg, network, &path, Some(id), manager).await?;
    let RelayMessage::HelloAck { generation } = net::receive(&mut ws).await? else {
        anyhow::bail!("missing control acknowledgement")
    };
    Ok((ws, generation))
}
async fn source(
    cfg: &ServerConfig,
    network: &str,
    target: &str,
    id: Option<&Identity>,
) -> Result<net::Ws> {
    let path = format!("/networks/{network}/connect/{target}");
    authed(cfg, network, &path, id, None).await
}
async fn incoming(ws: &mut net::Ws) -> Result<String> {
    let RelayMessage::Incoming { session_id } = net::receive(ws).await? else {
        anyhow::bail!("missing incoming session")
    };
    Ok(session_id)
}
async fn attach(
    cfg: &ServerConfig,
    network: &str,
    target: &str,
    generation: &str,
    sid: &str,
) -> Result<net::Ws> {
    let mut ws = socket(
        cfg,
        &format!("/networks/{network}/attach/{target}/{generation}/{sid}"),
    )
    .await?;
    match net::receive(&mut ws).await? {
        RelayMessage::Connected => Ok(ws),
        RelayMessage::Error { code, message } => anyhow::bail!("{code}: {message}"),
        _ => anyhow::bail!("invalid attach result"),
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_routes_only_proven_members_and_keeps_binding_and_resource_limits() -> Result<()> {
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
        let link = xrun::relay::deployment_link(&cfg)?;
        assert_eq!(link, xrun::relay::deployment_link(&cfg)?);
        let keys = crypto::load_or_create_server(&cfg)?;
        let net = network(&temp.path().join("manager"), &cfg)?;
        let other = network(&temp.path().join("other"), &cfg)?;
        let n = net.id.clone();
        let target = net.target.device_id.clone();
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
        // Only the random route is accepted; there is no roster API or database.
        for url in [
            format!("{}/networks/{n}/status", cfg.urls()[0]),
            format!("{}/wrong/networks/{n}/status", cfg.urls()[0]),
            format!("{}/networks/{n}/roster", xrun::relay::addresses(&cfg)?[0]),
        ] {
            assert_eq!(
                client
                    .get(url)
                    .header("x-xrun-version", VERSION)
                    .send()
                    .await?
                    .status(),
                reqwest::StatusCode::NOT_FOUND
            );
        }

        // A routing slot requires a fresh proof from that device's own key.
        let control_path = format!("/networks/{n}/control");
        let mut anonymous = authed(&cfg, &n, &control_path, None, None).await?;
        refused(&mut anonymous, "UNAUTHENTICATED").await?;
        let (mut ctl, generation) = control(&cfg, &n, &net.target, None).await?;
        let takeovers = [
            // Another member's certificate cannot claim the target's ID.
            forged(&cfg, &control_path, |nonce| {
                let mut proof = Proof::create(&net.source, &n, &control_path, nonce, None)?;
                proof.device_id = target.clone();
                Ok(proof)
            })
            .await?,
            // The target's public certificate is useless without its key.
            forged(&cfg, &control_path, |nonce| {
                let mut proof = Proof::create(&net.target, &n, &control_path, nonce, None)?;
                proof.signature =
                    Proof::create(&net.source, &n, &control_path, nonce, None)?.signature;
                Ok(proof)
            })
            .await?,
            // Members of another network cannot use this network ID.
            forged(&cfg, &control_path, |nonce| {
                Proof::create(&other.target, &n, &control_path, nonce, None)
            })
            .await?,
            // A proof is bound to its challenge and path.
            forged(&cfg, &control_path, |_| {
                Proof::create(&net.target, &n, &control_path, "replayed", None)
            })
            .await?,
        ];
        for mut ws in takeovers {
            refused(&mut ws, "UNAUTHENTICATED").await?;
        }
        let status_path = format!("/networks/{n}/status");
        let mut hidden = authed(&cfg, &n, &status_path, None, None).await?;
        refused(&mut hidden, "UNAUTHENTICATED").await?;
        let mut status = authed(&cfg, &n, &status_path, Some(&net.source), None).await?;
        let RelayMessage::Status { devices } = net::receive(&mut status).await? else {
            anyhow::bail!("missing routes")
        };
        assert_eq!(devices, vec![target.clone()]);
        drop(status);

        // The original control still receives sessions after failed takeovers.
        let mut cli = source(&cfg, &n, &target, Some(&net.source)).await?;
        let sid = incoming(&mut ctl).await?;
        assert!(attach(&cfg, &n, "other1", &generation, &sid).await.is_err());
        assert!(
            attach(&cfg, &n, &target, &"0".repeat(32), &sid)
                .await
                .is_err()
        );
        let mut data = attach(&cfg, &n, &target, &generation, &sid).await?;
        assert!(matches!(
            net::receive(&mut cli).await?,
            RelayMessage::Connected
        ));
        assert!(attach(&cfg, &n, &target, &generation, &sid).await.is_err());
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
        // The same device reconnecting invalidates attachments and old tunnels.
        let (mut new_control, new_generation) = control(&cfg, &n, &net.target, None).await?;
        assert_ne!(generation, new_generation);
        let closed = tokio::time::timeout(Duration::from_secs(2), cli.next()).await?;
        assert!(closed.is_none() || matches!(closed, Some(Ok(Message::Close(_))) | Some(Err(_))));
        drop((data, cli, ctl));
        let mut early = source(&cfg, &n, &target, Some(&net.source)).await?;
        let sid = incoming(&mut new_control).await?;
        early.close(None).await?;
        drop(early);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            attach(&cfg, &n, &target, &new_generation, &sid)
                .await
                .is_err()
        );

        // Pairing without a member certificate reaches only the root-key holder.
        let mut pairing = source(&cfg, &n, &target, None).await?;
        refused(&mut pairing, "UNAUTHENTICATED").await?;
        let mut pretender = forged(&cfg, &control_path, |nonce| {
            let mut proof = Proof::create(&net.source, &n, &control_path, nonce, None)?;
            proof.manager_signature = Some(proof.signature.clone());
            Ok(proof)
        })
        .await?;
        refused(&mut pretender, "UNAUTHENTICATED").await?;
        let manager = net.manager_identity.device_id.clone();
        let (mut manager_control, _) =
            control(&cfg, &n, &net.manager_identity, Some(&net.manager)).await?;
        let mut pending = vec![];
        for _ in 0..4 {
            pending.push(source(&cfg, &n, &manager, None).await?);
            incoming(&mut manager_control).await?;
        }
        let mut limit = source(&cfg, &n, &manager, None).await?;
        refused(&mut limit, "SESSION_LIMIT").await?;
        drop((pending, limit, manager_control));
        tokio::time::sleep(Duration::from_millis(300)).await;

        let mut pending = vec![];
        for _ in 0..16 {
            pending.push(source(&cfg, &n, &target, Some(&net.source)).await?);
            incoming(&mut new_control).await?;
        }
        let mut limit = source(&cfg, &n, &target, Some(&net.source)).await?;
        refused(&mut limit, "SESSION_LIMIT").await?;
        drop((pending, limit));
        assert!(!cfg.data_dir.join("relay.db").exists());
        drop(server);
        assert_eq!(link, xrun::relay::deployment_link(&cfg)?);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
