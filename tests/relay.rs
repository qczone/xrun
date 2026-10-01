use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use xrun::{
    config::{Identity, ServerConfig},
    crypto, net,
    protocol::*,
    store::{Invitation, ServerStore},
};

async fn register(
    cfg: &ServerConfig,
    keys: &crypto::ServerKeys,
    store: &ServerStore,
    name: &str,
) -> Result<Identity> {
    let token = store.invite(&Invitation {
        inviter_id: None,
        allow: false,
        admin: false,
    })?;
    let (key_pem, csr) = crypto::new_device_request()?;
    let client = crypto::http_client(&keys.ca_pem, None)?;
    let body = PairRequest {
        token,
        name: name.into(),
        csr_base64: STANDARD.encode(csr),
    };
    // The release check runs before consuming a pairing token.
    let wrong = client
        .post(format!("{}/pair", cfg.urls()[0]))
        .header("x-xrun-version", "incompatible")
        .json(&body)
        .send()
        .await?;
    assert!(!wrong.status().is_success());
    assert!(wrong.text().await?.contains("VERSION_MISMATCH"));
    let response = client
        .post(format!("{}/pair", cfg.urls()[0]))
        .header("x-xrun-version", VERSION)
        .json(&body)
        .send()
        .await?;
    assert!(response.status().is_success());
    let response: PairResponse = response.json().await?;
    let renewal = client
        .post(format!("{}/pair", cfg.urls()[0]))
        .header("x-xrun-version", VERSION)
        .json(&PairRequest {
            token: String::new(),
            name: name.into(),
            csr_base64: STANDARD.encode(crypto::renew_device_request(&key_pem)?),
        })
        .send()
        .await?;
    assert!(renewal.status().is_success());
    assert_eq!(
        renewal.json::<PairResponse>().await?.device_id,
        response.device_id
    );
    Ok(Identity {
        device_id: response.device_id,
        name: response.name,
        addresses: cfg.urls(),
        ca_pem: keys.ca_pem.clone(),
        cert_pem: response.cert_pem,
        key_pem,
        registration: response.registration,
    })
}
async fn socket(id: &Identity, path: &str) -> Result<(net::Ws, String)> {
    let address = id.addresses[0].clone();
    Ok((
        net::websocket_at(&address, path, crypto::client_tls_config(id)?).await?,
        address,
    ))
}
async fn control(id: &Identity) -> Result<net::Ws> {
    let (mut ws, _) = socket(id, "/daemon").await?;
    net::send(
        &mut ws,
        &Control::Hello {
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
        net::receive::<Control>(&mut ws).await?,
        Control::HelloAck
    ));
    Ok(ws)
}
async fn source(id: &Identity, target: &Identity) -> Result<net::Ws> {
    Ok(
        socket(id, &format!("/devices/{}/session", target.device_id))
            .await?
            .0,
    )
}
async fn session_id(control: &mut net::Ws, source: &Identity) -> Result<String> {
    let Control::SessionRequest {
        session_id,
        source_device_id,
    } = net::receive::<Control>(control).await?
    else {
        anyhow::bail!("expected session request")
    };
    assert_eq!(source_device_id, source.device_id);
    Ok(session_id)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_authentication_binding_and_limits() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let port = std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port();
    let cfg = ServerConfig {
        port,
        addresses: vec![format!("127.0.0.1:{port}")],
        manual: true,
        no_detect: true,
        data_dir: temp.path().join("server"),
    };
    let keys = crypto::load_or_create_server(&cfg)?;
    let store = ServerStore::open(&cfg.data_dir.join("server.db"))?;
    let server = tokio::spawn(xrun::server::run(cfg.clone()));
    tokio::time::timeout(Duration::from_secs(5), async {
        while tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            assert!(!server.is_finished(), "test Server exited during startup");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    assert!(
        crypto::discover_ca(&cfg.urls()[0], &"a".repeat(52))
            .await
            .is_err()
    );
    let a = register(&cfg, &keys, &store, "target1").await?;
    let b = register(&cfg, &keys, &store, "source1").await?;
    let c = register(&cfg, &keys, &store, "other1").await?;
    let mut ctl = control(&a).await?;
    let mut cli = source(&b, &a).await?;
    let sid = session_id(&mut ctl, &b).await?;
    let path = format!("/daemon/sessions/{sid}");
    let wrong = socket(&c, &path).await;
    assert!(wrong.is_err());
    let mut data = socket(&a, &path).await?.0;
    assert!(socket(&a, &path).await.is_err());
    let raw = "{\"opaque\":\"unchanged\",\"secret\":123}";
    data.send(Message::Text(raw.into())).await?;
    let message = tokio::time::timeout(Duration::from_secs(2), cli.next())
        .await?
        .context("source closed")??;
    assert_eq!(message, Message::Text(raw.into()));
    // Replacing control invalidates all sessions of its previous generation.
    let mut new_control = control(&a).await?;
    let closed = tokio::time::timeout(Duration::from_secs(2), cli.next()).await?;
    assert!(closed.is_none() || matches!(closed, Some(Ok(Message::Close(_))) | Some(Err(_))));
    drop(data);
    drop(cli);
    drop(ctl);
    // A source disconnect makes the outstanding target connection unusable.
    let mut early = source(&b, &a).await?;
    let sid = session_id(&mut new_control, &b).await?;
    early.close(None).await?;
    drop(early);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        socket(&a, &format!("/daemon/sessions/{sid}"))
            .await
            .is_err()
    );
    let mut pending = vec![];
    for _ in 0..16 {
        pending.push(source(&b, &a).await?);
        session_id(&mut new_control, &b).await?;
    }
    let limit = source(&b, &a).await;
    assert!(limit.is_err());
    assert!(limit.err().unwrap().to_string().contains("SESSION_LIMIT"));
    drop(pending);
    tokio::time::sleep(Duration::from_millis(100)).await;
    // Revocation is checked by registered key, not an exclusive leaf fingerprint.
    let mut revoked = store.get(&c.device_id)?.unwrap();
    revoked.device.revoked = true;
    store.save(&revoked)?;
    let response = crypto::http_client(&c.ca_pem, Some(&c))?
        .get(format!("{}/devices", cfg.urls()[0]))
        .header("x-xrun-version", VERSION)
        .send()
        .await?;
    assert!(response.text().await?.contains("DEVICE_REVOKED"));
    // Public pairing has a bounded per-IP rate without blocking authenticated reads.
    let client = crypto::http_client(&keys.ca_pem, None)?;
    let mut limited = false;
    for _ in 0..61 {
        let response = client
            .post(format!("{}/pair", cfg.urls()[0]))
            .header("x-xrun-version", VERSION)
            .json(&PairRequest {
                token: String::new(),
                name: "rate1".into(),
                csr_base64: "!".into(),
            })
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            assert!(response.text().await?.contains("RATE_LIMITED"));
            limited = true;
            break;
        }
    }
    assert!(limited, "pairing rate limit did not apply");
    server.abort();
    Ok(())
}
