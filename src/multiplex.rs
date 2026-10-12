//! Independent operation streams over one authenticated encrypted device pair.
use crate::{
    error::ErrorCode,
    net::{self, Ws},
    protocol::*,
    secure,
};
use anyhow::{Context, Result, bail};
use std::{
    future::poll_fn,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context as TaskContext, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot},
    time::Instant,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::protocol::{Role, WebSocketConfig},
};
use tokio_util::compat::{Compat, FuturesAsyncReadCompatExt, TokioAsyncReadCompatExt};
const MAX_STREAMS: usize = 32;
const IDLE: Duration = Duration::from_secs(300);
struct Shared {
    active: AtomicUsize,
    closed: AtomicBool,
    last: Mutex<Instant>,
    changed: Notify,
    stop: Notify,
}
#[derive(Clone)]
pub(crate) struct Channel {
    shared: Arc<Shared>,
    requests: mpsc::Sender<Open>,
    slots: Arc<Semaphore>,
}
struct OpenWake(Arc<Shared>);
impl Drop for OpenWake {
    fn drop(&mut self) {
        self.0.changed.notify_one();
    }
}
struct Open {
    reply: oneshot::Sender<Result<Ws>>,
    permit: OwnedSemaphorePermit,
}
struct ChannelIo {
    inner: net::Io,
    blocked: Arc<AtomicBool>,
}
impl AsyncRead for ChannelIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for ChannelIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, bytes);
        self.blocked.store(result.is_pending(), Ordering::Release);
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        self.blocked.store(result.is_pending(), Ordering::Release);
        result
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
struct Stream {
    inner: Compat<yamux::Stream>,
    shared: Arc<Shared>,
    _permit: Option<OwnedSemaphorePermit>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.shared.active.fetch_sub(1, Ordering::AcqRel);
        *self.shared.last.lock().unwrap() = Instant::now();
        self.shared.changed.notify_one();
    }
}
impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}
impl Channel {
    pub(crate) fn alive(&self) -> bool {
        !self.shared.closed.load(Ordering::Acquire)
    }
    pub(crate) fn active(&self) -> usize {
        self.shared.active.load(Ordering::Acquire)
    }
    pub(crate) fn idle_for(&self) -> Duration {
        self.shared.last.lock().unwrap().elapsed()
    }
    pub(crate) fn close(&self) {
        self.shared.closed.store(true, Ordering::Release);
        self.shared.stop.notify_one();
    }
    pub(crate) async fn open(&self) -> Result<Ws> {
        if !self.alive() {
            bail!(ErrorCode::ConnectionClosed.error("encrypted channel closed"));
        }
        let permit =
            self.slots.clone().try_acquire_owned().context(
                ErrorCode::DeviceBusy.error("device pair already has 32 active operations"),
            )?;
        let (reply, response) = oneshot::channel();
        let _wake = OpenWake(self.shared.clone());
        self.requests
            .send(Open { reply, permit })
            .await
            .context(ErrorCode::ConnectionClosed.error("encrypted channel closed"))?;
        response
            .await
            .context(ErrorCode::ConnectionClosed.error("encrypted channel closed"))?
    }
}
async fn framed(
    stream: yamux::Stream,
    shared: Arc<Shared>,
    permit: Option<OwnedSemaphorePermit>,
    role: Role,
) -> Ws {
    shared.active.fetch_add(1, Ordering::AcqRel);
    shared.changed.notify_one();
    let io = Stream {
        inner: stream.compat(),
        shared,
        _permit: permit,
    };
    WebSocketStream::from_raw_socket(
        net::SocketIo::new(Box::new(io)),
        role,
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(MAX_MESSAGE))
                .max_frame_size(Some(MAX_MESSAGE)),
        ),
    )
    .await
}
pub(crate) fn start(ws: Ws, mode: yamux::Mode) -> (Channel, mpsc::Receiver<Ws>) {
    let cache = if mode == yamux::Mode::Client {
        ws.get_ref().relay.as_ref().and_then(|r| r.cache.clone())
    } else {
        None
    };
    let shared = Arc::new(Shared {
        active: AtomicUsize::new(0),
        closed: AtomicBool::new(false),
        last: Mutex::new(Instant::now()),
        changed: Notify::new(),
        stop: Notify::new(),
    });
    let (requests, mut openings) = mpsc::channel::<Open>(MAX_STREAMS);
    let (incoming, receiver) = mpsc::channel(MAX_STREAMS);
    let channel = Channel {
        shared: shared.clone(),
        requests,
        slots: Arc::new(Semaphore::new(MAX_STREAMS)),
    };
    let mut config = yamux::Config::default();
    config.set_max_num_streams(MAX_STREAMS);
    config.set_max_connection_receive_window(Some(8 * 1024 * 1024));
    config.set_split_send_size(16 * 1024);
    let blocked = Arc::new(AtomicBool::new(false));
    let io = ChannelIo {
        inner: secure::stream_transport(ws),
        blocked: blocked.clone(),
    };
    let mut connection = yamux::Connection::new(io.compat(), config, mode);
    tokio::spawn(async move {
        enum Event {
            Inbound(Option<std::result::Result<yamux::Stream, yamux::ConnectionError>>),
            Outbound(
                Open,
                std::result::Result<yamux::Stream, yamux::ConnectionError>,
            ),
        }
        let mut pending = None::<Open>;
        let mut idle = false;
        let mut used = false;
        let result = async {
            loop {
                if pending.as_ref().is_some_and(|open| open.reply.is_closed()) { pending = None; }
                let inactive = shared.active.load(Ordering::Acquire) == 0 && pending.is_none();
                let current_idle = inactive && used;
                if current_idle != idle {
                    if let Some(cache) = &cache {
                        tokio::time::timeout(Duration::from_secs(5), cache.update(current_idle)).await??;
                    }
                    idle = current_idle;
                }
                if inactive && shared.last.lock().unwrap().elapsed() >= IDLE { break; }
                let expires = async {
                    if inactive {
                        let deadline = *shared.last.lock().unwrap() + IDLE;
                        tokio::time::sleep_until(deadline).await;
                    } else { std::future::pending::<()>().await; }
                };
                tokio::select! {
                    event = poll_fn(|cx| {
                        // Drive resets/closed stream cleanup before allocating a replacement.
                        // Yamux treats TooManyStreams as a fatal connection error. When its
                        // socket is backpressured, wait for that socket's registered waker.
                        if let Poll::Ready(inbound) = connection.poll_next_inbound(cx) {
                            return Poll::Ready(Event::Inbound(inbound));
                        }
                        if !blocked.load(Ordering::Acquire) && pending.is_some()
                            && let Poll::Ready(result) = connection.poll_new_outbound(cx) {
                            return Poll::Ready(Event::Outbound(pending.take().unwrap(), result));
                        }
                        Poll::Pending
                    }) => match event {
                        Event::Inbound(Some(Ok(stream))) => {
                            if mode == yamux::Mode::Client { bail!(ErrorCode::InvalidMessage.error("target cannot initiate business streams")); }
                            let ws = framed(stream, shared.clone(), None, Role::Server).await;
                            incoming.send(ws).await.map_err(|_| ErrorCode::ConnectionClosed.error("channel gateway closed"))?;
                        }
                        Event::Inbound(None) => break,
                        Event::Inbound(Some(Err(error))) => return Err(error.into()),
                        Event::Outbound(open, Ok(stream)) => {
                            let ws = framed(stream, shared.clone(), Some(open.permit), Role::Client).await;
                            let _ = open.reply.send(Ok(ws));
                        }
                        Event::Outbound(open, Err(error)) => { let _ = open.reply.send(Err(error.into())); }
                    },
                    open = openings.recv(), if pending.is_none() && mode == yamux::Mode::Client => match open {
                        Some(open) => { pending = Some(open); used = true; }
                        None => break,
                    },
                    _ = shared.changed.notified() => {},
                    _ = shared.stop.notified() => break,
                    _ = expires => {},
                }
            }
            Ok::<_, anyhow::Error>(())
        }.await;
        shared.closed.store(true, Ordering::Release);
        if let Err(error) = result {
            tracing::debug!(%error,"encrypted operation channel closed");
        }
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            poll_fn(|cx| connection.poll_close(cx)),
        )
        .await;
    });
    (channel, receiver)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    pub(crate) async fn pair() -> (Channel, Channel, mpsc::Receiver<Ws>) {
        let (a, b) = tokio::io::duplex(128 * 1024);
        let client =
            WebSocketStream::from_raw_socket(net::SocketIo::new(Box::new(a)), Role::Client, None)
                .await;
        let server =
            WebSocketStream::from_raw_socket(net::SocketIo::new(Box::new(b)), Role::Server, None)
                .await;
        let (client, _) = start(client, yamux::Mode::Client);
        let (server, incoming) = start(server, yamux::Mode::Server);
        (client, server, incoming)
    }
    #[tokio::test]
    async fn blocked_reader_does_not_block_other_operations_and_stream_drop_releases_capacity()
    -> Result<()> {
        let (client, server, mut incoming) = pair().await;
        let mut slow = client.open().await?;
        slow.send(tokio_tungstenite::tungstenite::Message::Binary(
            vec![1; 16].into(),
        ))
        .await?;
        let held = incoming.recv().await.context("first stream")?;
        let blocked = tokio::spawn(async move {
            slow.send(tokio_tungstenite::tungstenite::Message::Binary(
                vec![2; MAX_MESSAGE].into(),
            ))
            .await
        });
        let mut fast = client.open().await?;
        fast.send(tokio_tungstenite::tungstenite::Message::Text(
            "bounded independent operation".into(),
        ))
        .await?;
        let mut peer = incoming.recv().await.context("second stream")?;
        let message = tokio::time::timeout(Duration::from_secs(2), peer.next())
            .await?
            .context("fast operation")??;
        assert_eq!(
            message,
            tokio_tungstenite::tungstenite::Message::Text("bounded independent operation".into())
        );
        assert!(
            !blocked.is_finished(),
            "a slow reader must apply its own backpressure"
        );
        drop(held);
        blocked.abort();
        drop(peer);
        drop(fast);
        tokio::task::yield_now().await;
        assert_eq!(client.active(), 0);
        client.close();
        server.close();
        Ok(())
    }
    #[tokio::test]
    async fn a_device_pair_caps_streams_and_closed_channels_refuse_new_operations() -> Result<()> {
        let (client, server, _incoming) = pair().await;
        let mut held = vec![];
        for _ in 0..MAX_STREAMS {
            held.push(client.open().await?);
        }
        assert!(crate::error::is(
            &client.open().await.err().context("stream cap")?,
            ErrorCode::DeviceBusy
        ));
        for _ in 0..128 {
            held.pop();
            let reopened = tokio::time::timeout(Duration::from_secs(2), client.open()).await??;
            held.push(reopened);
        }
        drop(held);
        client.close();
        assert!(crate::error::is(
            &client.open().await.err().context("closed channel")?,
            ErrorCode::ConnectionClosed
        ));
        server.close();
        Ok(())
    }
}
