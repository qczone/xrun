use crate::error::ErrorCode;
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
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    task::{Context as TaskContext, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf},
    task::JoinHandle,
};
use tokio_tungstenite::tungstenite::{
    Message,
    protocol::{Role, WebSocketConfig},
};

const TLS_BUFFER_BYTES: usize = 256 * 1024;
const FLOW_ACK_THRESHOLD_BYTES: usize = 256 * 1024;

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
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FlowControl {
    Ack { bytes: usize },
}
fn tunnel(outer: Ws, flow_control: bool) -> Tunnel {
    let (inner, peer) = tokio::io::duplex(TLS_BUFFER_BYTES);
    let progress = Arc::new(Progress::default());
    let sent = progress.clone();
    let pump = tokio::spawn(async move {
        let _guard = PumpGuard(sent.clone());
        let (mut input, mut output) = tokio::io::split(peer);
        let (socket_tx, mut socket_rx) = outer.split();
        let socket_tx = tokio::sync::Mutex::new(socket_tx);
        let credit = tokio::sync::Semaphore::new(RELAY_WINDOW);
        let outstanding = AtomicUsize::new(0);
        let send = async {
            let mut buffer = vec![0; FILE_CHUNK];
            loop {
                let permit = if flow_control {
                    Some(
                        tokio::time::timeout(
                            RELAY_IDLE_TIMEOUT,
                            credit.acquire_many(FILE_CHUNK as u32),
                        )
                        .await??,
                    )
                } else {
                    None
                };
                let n = input.read(&mut buffer).await?;
                if n == 0 {
                    return Ok::<_, anyhow::Error>(());
                }
                if let Some(permit) = permit {
                    permit.forget();
                    credit.add_permits(FILE_CHUNK - n);
                    outstanding.fetch_add(n, Ordering::AcqRel);
                }
                tokio::time::timeout(RELAY_IDLE_TIMEOUT, async {
                    socket_tx
                        .lock()
                        .await
                        .send(Message::Binary(buffer[..n].to_vec().into()))
                        .await
                })
                .await??;
                sent.sent.fetch_add(n as u64, Ordering::Release);
                sent.waker.wake();
            }
        };
        let receive = async {
            let mut unacked = 0usize;
            while let Some(message) = socket_rx.next().await {
                match message? {
                    Message::Binary(bytes) if bytes.len() <= FILE_CHUNK => {
                        tokio::time::timeout(RELAY_IDLE_TIMEOUT, output.write_all(&bytes))
                            .await??;
                        // A small unacknowledged tail is bounded by the threshold.
                        // Keep it across requests; the window is much larger, so
                        // even a short request never has to wait for an ACK timer.
                        if flow_control {
                            unacked += bytes.len();
                        }
                        if flow_control && unacked >= FLOW_ACK_THRESHOLD_BYTES {
                            let ack = serde_json::to_string(&FlowControl::Ack { bytes: unacked })?;
                            unacked = 0;
                            tokio::time::timeout(RELAY_IDLE_TIMEOUT, async {
                                socket_tx.lock().await.send(Message::Text(ack.into())).await
                            })
                            .await??;
                        }
                    }
                    Message::Text(text) if flow_control => {
                        let FlowControl::Ack { bytes } = serde_json::from_str(&text)?;
                        if bytes == 0
                            || bytes > RELAY_WINDOW
                            || outstanding
                                .try_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                                    pending.checked_sub(bytes)
                                })
                                .is_err()
                        {
                            bail!(
                                ErrorCode::InvalidRelayMessage
                                    .error("invalid ciphertext acknowledgement")
                            )
                        }
                        credit.add_permits(bytes);
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Close(_) => return Ok::<_, anyhow::Error>(()),
                    _ => {
                        bail!(ErrorCode::InvalidRelayMessage.error("expected encrypted TLS bytes"))
                    }
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
    client_with_flow(outer, identity, target, false).await
}
pub(crate) async fn client_with_flow(
    outer: Ws,
    identity: &Identity,
    target: &str,
    flow_control: bool,
) -> Result<(Ws, Vec<u8>)> {
    let (tls, certificate) = connect_tls(
        outer,
        crypto::client_tls_config(identity)?,
        target,
        flow_control,
    )
    .await?;
    Ok((encrypted_websocket(tls, Role::Client).await, certificate))
}

async fn connect_tls(
    outer: Ws,
    config: Arc<rustls::ClientConfig>,
    target: &str,
    flow_control: bool,
) -> Result<(tokio_rustls::client::TlsStream<Tunnel>, Vec<u8>)> {
    let tls = tokio_rustls::TlsConnector::from(config)
        .connect(
            rustls::pki_types::ServerName::try_from(membership::device_name(target)?)?,
            tunnel(outer, flow_control),
        )
        .await?;
    let der = peer_certificate(tls.get_ref().1)
        .context(ErrorCode::Unauthenticated.error("target omitted its certificate"))?;
    let (actual, _) = crypto::peer_identity(&der)?;
    if actual != target {
        bail!(ErrorCode::IdentityMismatch.error("relay connected an unexpected device"))
    }
    Ok((tls, der))
}
fn peer_certificate(connection: &rustls::CommonState) -> Option<Vec<u8>> {
    connection
        .peer_certificates()
        .and_then(|certificates| certificates.first())
        .map(|certificate| certificate.to_vec())
}
async fn encrypted_websocket(
    io: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    role: Role,
) -> Ws {
    // Mutual or pinned TLS has already authenticated the endpoint. The inner
    // protocol uses raw WebSocket framing without another HTTP handshake.
    tokio_tungstenite::WebSocketStream::from_raw_socket(Box::new(io) as Io, role, Some(ws_config()))
        .await
}
// Anonymous pairing validates the pinned network root AND expected manager ID
// before any invitation token or CSR is sent through the encrypted channel.
pub async fn pairing_client(outer: Ws, pin: &str, manager: &str) -> Result<(Ws, String)> {
    pairing_client_with_flow(outer, pin, manager, false).await
}
pub(crate) async fn pairing_client_with_flow(
    outer: Ws,
    pin: &str,
    manager: &str,
    flow_control: bool,
) -> Result<(Ws, String)> {
    let (config, ca) = crypto::pinned_client_config(pin);
    let (tls, _) = connect_tls(outer, config, manager, flow_control).await?;
    let root = ca
        .lock()
        .unwrap()
        .clone()
        .context(ErrorCode::Unauthenticated.error("manager omitted the network root"))?;
    let ws = encrypted_websocket(tls, Role::Client).await;
    Ok((ws, root))
}
pub async fn server(outer: Ws, identity: &Identity) -> Result<(Ws, Option<Vec<u8>>)> {
    server_with_flow(outer, identity, false).await
}
pub(crate) async fn server_with_flow(
    outer: Ws,
    identity: &Identity,
    flow_control: bool,
) -> Result<(Ws, Option<Vec<u8>>)> {
    let tls = tokio_rustls::TlsAcceptor::from(crypto::peer_server_config(identity)?)
        .accept(tunnel(outer, flow_control))
        .await?;
    // Anonymous inner TLS is retained exclusively for the pairing handler.
    let peer = peer_certificate(tls.get_ref().1);
    let ws = encrypted_websocket(tls, Role::Server).await;
    Ok((ws, peer))
}
#[derive(Serialize, Deserialize)]
struct RosterExchange {
    version: String,
    protocol: ProtocolRange,
    roster: SignedRoster,
}
async fn receive_roster(ws: &mut Ws) -> Result<RosterExchange> {
    let value: serde_json::Value = net::receive(ws).await?;
    if let Ok(Data::Error { code, message }) = serde_json::from_value(value.clone()) {
        bail!(crate::error::CodedError::from_wire(code, message))
    }
    Ok(serde_json::from_value(value)?)
}
fn observe(cache: &RosterCache, network: &str, next: &SignedRoster) -> Result<()> {
    match cache.observe(network, next) {
        Ok(()) => Ok(()),
        // An authenticated peer may legitimately be behind. We keep our higher
        // version and validate its identity against that version below.
        Err(error) if crate::error::is(&error, ErrorCode::RosterRollback) => Ok(()),
        Err(error) => Err(error),
    }
}
pub async fn exchange_client(
    ws: &mut Ws,
    cache: &RosterCache,
    network: &str,
    cert: &[u8],
    target: &str,
    purpose: &Purpose,
) -> Result<SignedRoster> {
    let roster = cache.load(network)?;
    net::send(
        ws,
        &RosterExchange {
            version: VERSION.into(),
            protocol: ProtocolRange::CURRENT,
            roster,
        },
    )
    .await?;
    // Purpose carries no command or input. Pipeline it with our roster; the
    // caller still waits for verified peer state and Ready before any request.
    net::send(ws, purpose).await?;
    let peer = receive_roster(ws).await?;
    ProtocolRange::CURRENT.negotiate(peer.protocol)?;
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
) -> Result<(SignedRoster, String, u32)> {
    let peer = receive_roster(ws).await?;
    let selected = ProtocolRange::CURRENT.negotiate(peer.protocol)?;
    observe(cache, network, &peer.roster)?;
    let current = cache.load(network)?;
    let source = current.peer(cert, None)?.device_id.clone();
    net::send(
        ws,
        &RosterExchange {
            version: VERSION.into(),
            protocol: ProtocolRange::CURRENT,
            roster: current.clone(),
        },
    )
    .await?;
    Ok((current, source, selected))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "purpose", rename_all = "snake_case")]
pub enum Purpose {
    Execute,
    State,
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use std::time::Duration;
    use tokio_tungstenite::{WebSocketStream, tungstenite::protocol::Role};

    async fn wire() -> (Ws, Ws) {
        let (a, b) = tokio::io::duplex(2 * RELAY_WINDOW);
        (
            WebSocketStream::from_raw_socket(Box::new(a) as Io, Role::Client, None).await,
            WebSocketStream::from_raw_socket(Box::new(b) as Io, Role::Server, None).await,
        )
    }

    #[tokio::test]
    async fn slow_receiver_stops_ciphertext_until_it_acknowledges() -> Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let (outer, mut receiver) = wire().await;
            let mut sender = tunnel(outer, true);
            let producer = tokio::spawn(async move {
                sender.write_all(&vec![7; 2 * RELAY_WINDOW]).await?;
                sender.flush().await
            });
            for pass in 0..2 {
                let mut received = 0;
                while received < RELAY_WINDOW {
                    let Message::Binary(bytes) = receiver.next().await.context("closed")?? else {
                        bail!("unexpected frame")
                    };
                    assert!(bytes.iter().all(|b| *b == 7));
                    received += bytes.len();
                }
                assert_eq!(received, RELAY_WINDOW);
                if pass == 0 {
                    assert!(
                        tokio::time::timeout(Duration::from_millis(100), receiver.next())
                            .await
                            .is_err()
                    );
                }
                if pass == 0 {
                    net::send(&mut receiver, &FlowControl::Ack { bytes: received }).await?;
                }
            }
            producer.await??;
            Ok::<_, anyhow::Error>(())
        })
        .await?
    }

    #[tokio::test]
    async fn forged_credit_closes_the_tunnel() -> Result<()> {
        let (outer, mut relay) = wire().await;
        let mut stream = tunnel(outer, true);
        net::send(
            &mut relay,
            &FlowControl::Ack {
                bytes: RELAY_WINDOW + 1,
            },
        )
        .await?;
        let mut byte = [0];
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), stream.read(&mut byte)).await??,
            0
        );
        Ok(())
    }
}
