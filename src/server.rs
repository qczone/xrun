use crate::config::ServerConfig;
use anyhow::{Context, Result};
use axum::{Router, body::Body, http::StatusCode};
use std::{
    collections::HashMap,
    future::Future,
    net::IpAddr,
    pin::Pin,
    sync::{Arc, atomic::Ordering},
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tower::ServiceExt;

const MAX_CONNECTIONS: usize = 512;
const MAX_ANONYMOUS_PER_IP: usize = 16;
#[derive(Default)]
struct ConnectionLimits {
    total: usize,
    anonymous: HashMap<IpAddr, usize>,
}
// The permit belongs to the socket, including after a WebSocket upgrade.
#[derive(Clone)]
pub(crate) struct TransportPermit {
    limits: Arc<std::sync::Mutex<ConnectionLimits>>,
    ip: IpAddr,
    anonymous: Arc<std::sync::atomic::AtomicBool>,
}
impl TransportPermit {
    pub(crate) fn authenticated(&self) {
        let mut limits = self.limits.lock().unwrap();
        if self.anonymous.swap(false, Ordering::SeqCst) {
            limits.release_anonymous(self.ip);
        }
    }
}
type StopSignal = Pin<Box<dyn Future<Output = ()> + Send>>;
fn stop_signal(mut receiver: tokio::sync::watch::Receiver<()>) -> StopSignal {
    Box::pin(async move {
        let _ = receiver.changed().await;
    })
}
struct LimitedTcp {
    inner: tokio::net::TcpStream,
    permit: TransportPermit,
    read_stop: StopSignal,
    write_stop: StopSignal,
    stopped: bool,
}
impl LimitedTcp {
    fn new(
        inner: tokio::net::TcpStream,
        ip: IpAddr,
        limits: Arc<std::sync::Mutex<ConnectionLimits>>,
        lifetime: &tokio::sync::watch::Sender<()>,
    ) -> Option<Self> {
        {
            let mut counts = limits.lock().unwrap();
            if counts.total >= MAX_CONNECTIONS
                || counts.anonymous.get(&ip).copied().unwrap_or(0) >= MAX_ANONYMOUS_PER_IP
            {
                return None;
            }
            counts.total += 1;
            *counts.anonymous.entry(ip).or_default() += 1;
        }
        Some(Self {
            inner,
            read_stop: stop_signal(lifetime.subscribe()),
            write_stop: stop_signal(lifetime.subscribe()),
            stopped: false,
            permit: TransportPermit {
                limits,
                ip,
                anonymous: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            },
        })
    }
    fn stopped(&mut self, cx: &mut TaskContext<'_>, read: bool) -> bool {
        if !self.stopped {
            self.stopped = if read {
                self.read_stop.as_mut().poll(cx).is_ready()
            } else {
                self.write_stop.as_mut().poll(cx).is_ready()
            };
        }
        self.stopped
    }
}
impl ConnectionLimits {
    fn release_anonymous(&mut self, ip: IpAddr) {
        let count = self.anonymous.get_mut(&ip).unwrap();
        *count -= 1;
        if *count == 0 {
            self.anonymous.remove(&ip);
        }
    }
}
impl Drop for LimitedTcp {
    fn drop(&mut self) {
        let mut counts = self.permit.limits.lock().unwrap();
        counts.total -= 1;
        if self.permit.anonymous.swap(false, Ordering::SeqCst) {
            counts.release_anonymous(self.permit.ip);
        }
    }
}
impl AsyncRead for LimitedTcp {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.stopped(cx, true) {
            return Poll::Ready(Err(std::io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for LimitedTcp {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.stopped(cx, false) {
            return Poll::Ready(Err(std::io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        if self.stopped(cx, false) {
            return Poll::Ready(Err(std::io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.stopped(cx, false) {
            return Poll::Ready(Err(std::io::ErrorKind::ConnectionAborted.into()));
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
pub fn instance_lock(dir: &std::path::Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(dir)?;
    crate::config::restrict_dir(dir)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("server.lock"))?;
    lock.try_lock()
        .context("SERVER_RUNNING: server already running")?;
    Ok(lock)
}
pub async fn run(config: ServerConfig) -> Result<()> {
    crate::relay::run(config).await
}

pub(crate) async fn serve_http(
    port: u16,
    acceptor: tokio_rustls::TlsAcceptor,
    router: Router,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).await?;
    // The listener owns this sender. Dropping it wakes and closes sockets even
    // after Hyper has handed them to detached WebSocket upgrade handlers.
    let (lifetime, _) = tokio::sync::watch::channel(());
    tracing::info!(port, "xrun relay listening");
    let limits = Arc::new(std::sync::Mutex::new(ConnectionLimits::default()));
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                tracing::warn!(%error, "accept failed; retrying");
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
        };
        if let Err(error) = tcp.set_nodelay(true) {
            tracing::warn!(%error, "cannot configure accepted socket");
            continue;
        }
        let Some(tcp) = LimitedTcp::new(tcp, peer.ip(), limits.clone(), &lifetime) else {
            continue;
        };
        let acceptor = acceptor.clone();
        let router = router.clone();
        tokio::spawn(async move {
            let tls =
                match tokio::time::timeout(Duration::from_secs(10), acceptor.accept(tcp)).await {
                    Ok(Ok(tls)) => tls,
                    Ok(Err(error)) => {
                        tracing::warn!(%peer,%error,"TLS authentication failed");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!(%peer,"TLS handshake timed out");
                        return;
                    }
                };
            let permit = tls.get_ref().0.permit.clone();
            let service =
                hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                    let router = router.clone();
                    let permit = permit.clone();
                    async move {
                        let mut req = req.map(Body::new);
                        req.extensions_mut().insert(peer);
                        req.extensions_mut().insert(permit);
                        let mut response = router.oneshot(req).await?;
                        if response.status() != StatusCode::SWITCHING_PROTOCOLS {
                            response.headers_mut().insert(
                                axum::http::header::CONNECTION,
                                axum::http::HeaderValue::from_static("close"),
                            );
                        }
                        Ok::<_, std::convert::Infallible>(response)
                    }
                });
            // Bound slow request bodies as well as idle HTTP connections.
            let mut builder = hyper::server::conn::http1::Builder::new();
            builder
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(Duration::from_secs(10));
            let _ = tokio::time::timeout(
                Duration::from_secs(30),
                builder
                    .serve_connection(hyper_util::rt::TokioIo::new(tls), service)
                    .with_upgrades(),
            )
            .await;
        });
    }
}
