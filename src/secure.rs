use crate::{
    config::Identity,
    crypto,
    membership::{self, RosterCache, SignedRoster},
    net::{self, Io, Ws},
    protocol::*,
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf},
    task::JoinHandle,
};
use tokio_tungstenite::{
    accept_async_with_config, client_async_with_config,
    tungstenite::{Message, protocol::WebSocketConfig},
};

// Every direction has at most the duplex capacity plus one 64 KiB frame. The
// task belongs to the stream; closing/cancelling the inner TLS session closes
// the outer relay socket instead of leaving a background pump alive.
struct Tunnel {
    inner: DuplexStream,
    pump: JoinHandle<()>,
    progress: Arc<Progress>,
    written: u64,
}
#[derive(Default)]
struct Progress {
    sent: AtomicU64,
    closed: AtomicBool,
    waker: futures_util::task::AtomicWaker,
}
struct PumpGuard(Arc<Progress>);
impl Drop for PumpGuard {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::Release);
        self.0.waker.wake();
    }
}
impl Drop for Tunnel {
    fn drop(&mut self) {
        self.pump.abort();
    }
}
impl AsyncRead for Tunnel {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for Tunnel {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match Pin::new(&mut self.inner).poll_write(cx, buf) {
            Poll::Ready(Ok(n)) => {
                self.written += n as u64;
                Poll::Ready(Ok(n))
            }
            result => result,
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        self.progress.waker.register(cx.waker());
        if self.progress.sent.load(Ordering::Acquire) >= self.written {
            Poll::Ready(Ok(()))
        } else if self.progress.closed.load(Ordering::Acquire) {
            Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
        } else {
            Poll::Pending
        }
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        std::task::ready!(self.as_mut().poll_flush(cx))?;
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
fn tunnel(outer: Ws) -> Tunnel {
    let (inner, peer) = tokio::io::duplex(256 * 1024);
    let progress = Arc::new(Progress::default());
    let sent = progress.clone();
    let pump = tokio::spawn(async move {
        let _guard = PumpGuard(sent.clone());
        let (mut input, mut output) = tokio::io::split(peer);
        let (mut socket_tx, mut socket_rx) = outer.split();
        let send = async {
            let mut buffer = vec![0; FILE_CHUNK];
            loop {
                let n = input.read(&mut buffer).await?;
                if n == 0 {
                    return Ok::<_, anyhow::Error>(());
                }
                tokio::time::timeout(
                    Duration::from_secs(300),
                    socket_tx.send(Message::Binary(buffer[..n].to_vec().into())),
                )
                .await??;
                sent.sent.fetch_add(n as u64, Ordering::Release);
                sent.waker.wake();
            }
        };
        let receive = async {
            while let Some(message) = socket_rx.next().await {
                match message? {
                    Message::Binary(bytes) if bytes.len() <= FILE_CHUNK => {
                        tokio::time::timeout(Duration::from_secs(300), output.write_all(&bytes))
                            .await??;
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Close(_) => return Ok::<_, anyhow::Error>(()),
                    _ => bail!("INVALID_RELAY_MESSAGE: expected encrypted TLS bytes"),
                }
            }
            Ok(())
        };
        let result = tokio::select! {result=send=>result,result=receive=>result};
        if let Err(error) = result {
            tracing::debug!(%error,"encrypted tunnel closed");
        }
    });
    Tunnel {
        inner,
        pump,
        progress,
        written: 0,
    }
}
fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE))
}
pub async fn client(outer: Ws, identity: &Identity, target: &str) -> Result<(Ws, Vec<u8>)> {
    let tls = tokio_rustls::TlsConnector::from(crypto::client_tls_config(identity)?)
        .connect(
            rustls::pki_types::ServerName::try_from(membership::device_name(target)?)?,
            tunnel(outer),
        )
        .await?;
    let der = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .context("UNAUTHENTICATED: target omitted its certificate")?
        .to_vec();
    let (actual, _) = crypto::peer_identity(&der)?;
    if actual != target {
        bail!("IDENTITY_MISMATCH: relay connected an unexpected device")
    }
    let (ws, _) = client_async_with_config(
        "wss://peer.xrun/xrun",
        Box::new(tls) as Io,
        Some(ws_config()),
    )
    .await?;
    Ok((ws, der))
}
// Anonymous pairing validates the pinned network root AND expected manager ID
// before any invitation token or CSR is sent through the encrypted channel.
pub async fn pairing_client(outer: Ws, pin: &str, manager: &str) -> Result<(Ws, String)> {
    let (config, ca) = crypto::pinned_client_config(pin);
    let tls = tokio_rustls::TlsConnector::from(config)
        .connect(
            rustls::pki_types::ServerName::try_from(membership::device_name(manager)?)?,
            tunnel(outer),
        )
        .await?;
    let der = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .context("UNAUTHENTICATED: manager omitted its certificate")?;
    if crypto::peer_identity(der)?.0 != manager {
        bail!("IDENTITY_MISMATCH: pairing peer is not the invited manager")
    }
    let root = ca
        .lock()
        .unwrap()
        .clone()
        .context("UNAUTHENTICATED: manager omitted the network root")?;
    let (ws, _) = client_async_with_config(
        "wss://peer.xrun/xrun",
        Box::new(tls) as Io,
        Some(ws_config()),
    )
    .await?;
    Ok((ws, root))
}
pub async fn server(outer: Ws, identity: &Identity) -> Result<(Ws, Option<Vec<u8>>)> {
    let tls = tokio_rustls::TlsAcceptor::from(crypto::peer_server_config(identity)?)
        .accept(tunnel(outer))
        .await?;
    let peer = tls
        .get_ref()
        .1
        .peer_certificates()
        .and_then(|c| c.first())
        .map(|c| c.to_vec());
    let ws = accept_async_with_config(Box::new(tls) as Io, Some(ws_config())).await?;
    Ok((ws, peer))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RosterExchange {
    version: String,
    roster: SignedRoster,
}
fn observe(cache: &RosterCache, network: &str, next: &SignedRoster) -> Result<()> {
    match cache.observe(network, next) {
        Ok(()) => Ok(()),
        // An authenticated peer may legitimately be behind. We keep our higher
        // version and validate its identity against that version below.
        Err(error) if error.to_string().starts_with("ROSTER_ROLLBACK") => Ok(()),
        Err(error) => Err(error),
    }
}
pub async fn exchange_client(
    ws: &mut Ws,
    cache: &RosterCache,
    network: &str,
    cert: &[u8],
    target: &str,
) -> Result<SignedRoster> {
    let roster = cache.load(network)?;
    net::send(
        ws,
        &RosterExchange {
            version: VERSION.into(),
            roster,
        },
    )
    .await?;
    let peer: RosterExchange = net::receive(ws).await?;
    if peer.version != VERSION {
        bail!("VERSION_MISMATCH: peer release differs")
    }
    observe(cache, network, &peer.roster)?;
    let current = cache.load(network)?;
    current.peer(cert, Some(target))?;
    Ok(current)
}
pub async fn exchange_server(
    ws: &mut Ws,
    cache: &RosterCache,
    network: &str,
    cert: &[u8],
) -> Result<(SignedRoster, String)> {
    let peer: RosterExchange = net::receive(ws).await?;
    if peer.version != VERSION {
        bail!("VERSION_MISMATCH: peer release differs")
    }
    observe(cache, network, &peer.roster)?;
    let current = cache.load(network)?;
    let source = current.peer(cert, None)?.device_id.clone();
    net::send(
        ws,
        &RosterExchange {
            version: VERSION.into(),
            roster: current.clone(),
        },
    )
    .await?;
    Ok((current, source))
}
