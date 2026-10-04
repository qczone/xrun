mod common;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;
use xrun::{config::ServerConfig, crypto, net, protocol::VERSION, relay::RelayMessage};

struct Relay(tokio::task::JoinHandle<Result<()>>);
impl Drop for Relay {
    fn drop(&mut self) {
        self.0.abort();
    }
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
async fn control(cfg: &ServerConfig, id: &str) -> Result<(net::Ws, String)> {
    let mut ws = socket(cfg, "/networks/test/control").await?;
    net::send(
        &mut ws,
        &RelayMessage::Hello {
            device_id: id.into(),
        },
    )
    .await?;
    let RelayMessage::HelloAck { generation } = net::receive(&mut ws).await? else {
        anyhow::bail!("missing control acknowledgement")
    };
    Ok((ws, generation))
}
async fn source(cfg: &ServerConfig) -> Result<net::Ws> {
    socket(cfg, "/networks/test/connect/target1").await
}
async fn incoming(ws: &mut net::Ws) -> Result<String> {
    let RelayMessage::Incoming { session_id } = net::receive(ws).await? else {
        anyhow::bail!("missing incoming session")
    };
    Ok(session_id)
}
async fn attach(cfg: &ServerConfig, target: &str, generation: &str, sid: &str) -> Result<net::Ws> {
    let mut ws = socket(
        cfg,
        &format!("/networks/test/attach/{target}/{generation}/{sid}"),
    )
    .await?;
    match net::receive(&mut ws).await? {
        RelayMessage::Connected => Ok(ws),
        RelayMessage::Error { code, message } => anyhow::bail!("{code}: {message}"),
        _ => anyhow::bail!("invalid attach result"),
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relay_only_routes_connections_and_keeps_binding_and_resource_limits() -> Result<()> {
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
            format!("{}/networks/test/status", cfg.urls()[0]),
            format!("{}/wrong/networks/test/status", cfg.urls()[0]),
            format!("{}/networks/test/roster", xrun::relay::addresses(&cfg)?[0]),
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
        let (mut ctl, generation) = control(&cfg, "target1").await?;
        let mut status = socket(&cfg, "/networks/test/status").await?;
        let RelayMessage::Status { devices } = net::receive(&mut status).await? else {
            anyhow::bail!("missing routes")
        };
        assert_eq!(devices, vec!["target1"]);
        drop(status);
        let mut cli = source(&cfg).await?;
        let sid = incoming(&mut ctl).await?;
        assert!(attach(&cfg, "other1", &generation, &sid).await.is_err());
        assert!(
            attach(&cfg, "target1", &"0".repeat(32), &sid)
                .await
                .is_err()
        );
        let mut data = attach(&cfg, "target1", &generation, &sid).await?;
        assert!(matches!(
            net::receive(&mut cli).await?,
            RelayMessage::Connected
        ));
        assert!(attach(&cfg, "target1", &generation, &sid).await.is_err());
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
        // A new control connection invalidates attachments and old tunnels.
        let (mut new_control, new_generation) = control(&cfg, "target1").await?;
        assert_ne!(generation, new_generation);
        let closed = tokio::time::timeout(Duration::from_secs(2), cli.next()).await?;
        assert!(closed.is_none() || matches!(closed, Some(Ok(Message::Close(_))) | Some(Err(_))));
        drop((data, cli, ctl));
        let mut early = source(&cfg).await?;
        let sid = incoming(&mut new_control).await?;
        early.close(None).await?;
        drop(early);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            attach(&cfg, "target1", &new_generation, &sid)
                .await
                .is_err()
        );
        let mut pending = vec![];
        for _ in 0..16 {
            pending.push(source(&cfg).await?);
            incoming(&mut new_control).await?;
        }
        let mut limit = source(&cfg).await?;
        let RelayMessage::Error { code, .. } = net::receive(&mut limit).await? else {
            anyhow::bail!("missing limit error")
        };
        assert_eq!(code, "SESSION_LIMIT");
        drop((pending, limit));
        assert!(!cfg.data_dir.join("relay.db").exists());
        drop(server);
        assert_eq!(link, xrun::relay::deployment_link(&cfg)?);
        Ok::<_, anyhow::Error>(())
    })
    .await?
}
