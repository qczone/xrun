use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path, sync::Arc, time::Duration};
use tokio::io::AsyncReadExt;
use tokio_tungstenite::{
    client_async,
    tungstenite::{Message, client::IntoClientRequest, http::HeaderValue},
};
use xrun::{
    config::{Identity, ServerConfig},
    crypto,
    membership::{Manager, RosterCache},
    net::{self, Io, Ws},
    protocol::{MAX_FILE, Registration, sha256},
    secure,
};

const MARKER: &str = "xrun-cf-demo-plaintext-marker";

struct Probe {
    base: String,
    token: String,
    room: String,
    http: reqwest::Client,
}

impl Probe {
    async fn stats(&self) -> Result<Value> {
        Ok(self
            .http
            .get(format!("{}/rooms/{}/stats", self.base, self.room))
            .bearer_auth(&self.token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    async fn open(&self, side: &str) -> Result<Ws> {
        let url = url::Url::parse(&format!("{}/rooms/{}/{side}", self.base, self.room))?;
        let tcp = net::tcp(&url).await?;
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = tokio_rustls::TlsConnector::from(Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        ))
        .connect(
            rustls::pki_types::ServerName::try_from(
                url.host_str().context("missing host")?.to_owned(),
            )?,
            tcp,
        )
        .await?;
        let ws_url = url.as_str().replacen("https://", "wss://", 1);
        let mut request = ws_url.into_client_request()?;
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {}", self.token))?,
        );
        Ok(client_async(request, Box::new(tls) as Io).await?.0)
    }

    async fn pair(&self) -> Result<(Ws, Ws)> {
        let mut left = self.open("left").await?;
        ensure!(net::receive::<Value>(&mut left).await?["type"] == "waiting");
        let mut right = self.open("right").await?;
        ensure!(net::receive::<Value>(&mut left).await?["type"] == "ready");
        ensure!(net::receive::<Value>(&mut right).await?["type"] == "ready");
        Ok((left, right))
    }

    async fn empty(&self) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if self.stats().await?["sockets"]
                    .as_array()
                    .context("sockets")?
                    .is_empty()
                {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .context("relay did not clean up disconnected sockets")??;
        Ok(())
    }
}

async fn ping(ws: &mut Ws) -> Result<()> {
    let bytes = b"cf-hibernation-probe".to_vec();
    ws.send(Message::Ping(bytes.clone().into())).await?;
    tokio::time::timeout(Duration::from_secs(10), async {
        match ws.next().await.context("connection closed before pong")?? {
            Message::Pong(data) if data.as_ref() == bytes.as_slice() => Ok::<_, anyhow::Error>(()),
            message => bail!("unexpected ping response: {message:?}"),
        }
    })
    .await??;
    Ok(())
}

fn identities(dir: &Path) -> Result<(Manager, Identity, Identity)> {
    let transport = crypto::load_or_create_server(&ServerConfig {
        port: 9528,
        addresses: vec!["127.0.0.1:9528".into()],
        manual: true,
        no_detect: true,
        data_dir: dir.join("unused-relay-ca"),
    })?;
    let (manager, member, key_pem, cert_pem) = Manager::create(
        &dir.join("manager"),
        "probe1",
        vec!["https://127.0.0.1:9528".into()],
        transport.ca_pem,
    )?;
    let root = manager.roster()?.ca_pem;
    let source = Identity {
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
    let (key_pem, csr) = crypto::new_device_request()?;
    let paired = manager.pair(&manager.invite(false)?, "probe2", &csr)?;
    let target = Identity {
        network: None,
        device_id: paired.member.device_id,
        name: paired.member.name,
        addresses: vec![],
        ca_pem: root,
        cert_pem: paired.cert_pem,
        key_pem,
        registration: Registration {
            inviter_id: Some(source.device_id.clone()),
            allow_inviter: false,
        },
    };
    Ok((manager, source, target))
}

async fn run(base: String, token: String) -> Result<()> {
    ensure!(
        url::Url::parse(&base)?.scheme() == "https",
        "HTTPS endpoint required"
    );
    let probe = Probe {
        base: base.trim_end_matches('/').into(),
        token,
        room: uuid::Uuid::new_v4().to_string(),
        http: reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?,
    };
    let denied = probe
        .http
        .get(format!("{}/rooms/{}/stats", probe.base, probe.room))
        .send()
        .await?;
    ensure!(denied.status() == 401, "anonymous access was not rejected");
    println!("PASS anonymous requests rejected");

    let (mut left, right) = probe.pair().await?;
    ensure!(probe.open("left").await.is_err(), "duplicate side accepted");
    let before = probe.stats().await?;
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(15)).await;
        ping(&mut left).await?;
    }
    let after = probe.stats().await?;
    ensure!(
        before["bootId"] != after["bootId"],
        "Durable Object did not hibernate during idle test"
    );
    ensure!(after["sockets"].as_array().context("sockets")?.len() == 2);
    println!("PASS automatic ping/pong and real hibernation with both sockets retained");

    let dir = tempfile::tempdir()?;
    let (manager, source, target) = identities(dir.path())?;
    let roster = manager.roster()?;
    let network = &roster.roster.network_id;
    let source_cache = RosterCache::open(&dir.path().join("source.db"))?;
    let target_cache = RosterCache::open(&dir.path().join("target.db"))?;
    source_cache.observe(network, &roster)?;
    target_cache.observe(network, &roster)?;
    // Manager is offline throughout the end-to-end session.
    drop(manager);
    let content: Vec<u8> = MARKER
        .as_bytes()
        .iter()
        .copied()
        .cycle()
        .take(4 * 1024 * 1024)
        .collect();
    let hash = sha256(&content);
    let start = std::time::Instant::now();
    let server = async {
        let (mut ws, cert) = secure::server(right, &target).await?;
        let (_, peer) = secure::exchange_server(
            &mut ws,
            &target_cache,
            network,
            &cert.context("missing source certificate")?,
        )
        .await?;
        ensure!(peer == source.device_id);
        ensure!(net::receive::<Value>(&mut ws).await? == json!({"op":"exec","fixture":MARKER}));
        let mut child = xrun::process::spawn(
            &std::env::current_exe()?,
            &["--child".into()],
            dir.path(),
            &BTreeMap::new(),
            "cf-demo",
            false,
        )?;
        drop(child.stdin.take());
        let mut stdout = child.stdout.take().context("stdout")?;
        let mut stderr = child.stderr.take().context("stderr")?;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let (_, _, status) = tokio::try_join!(
            async { Ok::<_, anyhow::Error>(stdout.read_to_end(&mut out).await?) },
            async { Ok::<_, anyhow::Error>(stderr.read_to_end(&mut err).await?) },
            child.wait(),
        )?;
        child.reap().await?;
        ensure!(status.success() && err.is_empty());
        net::send(
            &mut ws,
            &json!({"stdout":String::from_utf8(out)?,"exit_code":status.code()}),
        )
        .await?;
        let upload: Value = net::receive(&mut ws).await?;
        ensure!(upload == json!({"op":"upload","size":content.len(),"sha256":hash}));
        let file = net::receive_file(&mut ws, content.len() as u64, &hash).await?;
        net::send(
            &mut ws,
            &json!({"op":"download","size":content.len(),"sha256":hash}),
        )
        .await?;
        net::send_file(&mut ws, file.as_file()).await?;
        ensure!(net::receive::<Value>(&mut ws).await? == json!({"type":"received"}));
        Ok::<_, anyhow::Error>(ws)
    };
    let client = async {
        let (mut ws, cert) = secure::client(left, &source, &target.device_id).await?;
        secure::exchange_client(&mut ws, &source_cache, network, &cert, &target.device_id).await?;
        net::send(&mut ws, &json!({"op":"exec","fixture":MARKER})).await?;
        let result: Value = net::receive(&mut ws).await?;
        ensure!(result == json!({"stdout":format!("{MARKER}\n"),"exit_code":0}));
        net::send(
            &mut ws,
            &json!({"op":"upload","size":content.len(),"sha256":hash}),
        )
        .await?;
        net::send_bytes(&mut ws, &content).await?;
        ensure!(
            net::receive::<Value>(&mut ws).await?
                == json!({"op":"download","size":content.len(),"sha256":hash})
        );
        // Delay the consumer while the remote end starts writing the response.
        tokio::time::sleep(Duration::from_millis(750)).await;
        ensure!(
            net::receive_bytes(&mut ws, content.len() as u64, &hash, MAX_FILE).await? == content
        );
        net::send(&mut ws, &json!({"type":"received"})).await?;
        Ok::<_, anyhow::Error>(ws)
    };
    let (server, client) = tokio::try_join!(server, client)?;
    println!("PASS existing mutual TLS, signed roster and subprocess output; manager offline");
    println!(
        "PASS 4 MiB upload/download SHA-256, delayed consumer; {} ms",
        start.elapsed().as_millis()
    );
    let metrics = probe.stats().await?;
    for socket in metrics["sockets"].as_array().context("sockets")? {
        ensure!(socket["bytes"].as_u64().context("bytes")? > content.len() as u64);
        ensure!(socket["frames"].as_u64().context("frames")? > 0);
        ensure!(
            socket["plaintextHits"] == 0,
            "plaintext marker reached the relay"
        );
    }
    println!("PASS relay received ciphertext without fixture plaintext: {metrics}");
    drop((server, client));
    probe.empty().await?;

    let (mut left, mut right) = probe.pair().await?;
    left.send(Message::Binary(vec![0; 64 * 1024 + 1].into()))
        .await?;
    for ws in [&mut left, &mut right] {
        let frame = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await?
            .context("missing limit response")??;
        ensure!(
            matches!(frame, Message::Close(Some(ref close)) if u16::from(close.code) == 1009),
            "oversized frame not rejected: {frame:?}"
        );
        ws.flush().await?;
    }
    drop((left, right));
    probe.empty().await?;
    println!("PASS oversized frame closes both sides; session cleaned up");

    let (mut left, mut right) = probe.pair().await?;
    ping(&mut left).await?;
    left.close(None).await?;
    ensure!(matches!(
        tokio::time::timeout(Duration::from_secs(10), right.next())
            .await?
            .context("peer closure")??,
        Message::Close(_)
    ));
    right.flush().await?;
    drop((left, right));
    probe.empty().await?;
    println!("PASS reconnect and peer close propagation");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.get(1).is_some_and(|arg| arg == "--child") {
        println!("{MARKER}");
        return Ok(());
    }
    ensure!(
        args.len() == 3,
        "usage: xrun-cloudflare-probe <https-endpoint> <token-file>"
    );
    let token = std::fs::read_to_string(&args[2])?.trim().to_owned();
    ensure!(!token.is_empty(), "empty token");
    tokio::time::timeout(Duration::from_secs(180), run(args[1].clone(), token)).await??;
    Ok(())
}
