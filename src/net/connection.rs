//! Socket IO keeps the outer relay version and its cache control together.
use super::{Io, Ws};
use crate::error::ErrorCode;
use anyhow::{Context, Result};
use std::{
    pin::Pin,
    task::{Context as TaskContext, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::{mpsc, oneshot},
};

/// Framed transport and metadata belonging to that exact relay connection.
pub struct SocketIo {
    inner: Io,
    pub(crate) relay: Option<RelayContext>,
}
impl SocketIo {
    /// Wrap a local or encrypted stream without assuming a relay protocol.
    pub fn new(inner: Io) -> Self {
        Self { inner, relay: None }
    }
    pub(crate) fn with_relay(inner: Io, relay: Option<RelayContext>) -> Self {
        Self { inner, relay }
    }
}
impl AsyncRead for SocketIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buffer)
    }
}
impl AsyncWrite for SocketIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buffer)
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

#[derive(Clone)]
pub(crate) struct RelayContext {
    pub protocol: u32,
    pub cache: Option<CacheControl>,
}
#[derive(Clone)]
pub(crate) struct CacheControl(mpsc::Sender<CacheRequest>);
pub(crate) struct CacheRequest {
    pub idle: bool,
    pub reply: oneshot::Sender<Result<()>>,
}
impl CacheControl {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<CacheRequest>) {
        let (sender, receiver) = mpsc::channel(1);
        (Self(sender), receiver)
    }
    async fn update(&self, idle: bool) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.0
            .send(CacheRequest { idle, reply })
            .await
            .context(ErrorCode::ConnectionClosed.error("relay cache controller closed"))?;
        response
            .await
            .context(ErrorCode::ConnectionClosed.error("relay did not acknowledge cache state"))?
    }
}

pub(crate) async fn cache_state(ws: &mut Ws, idle: bool) -> Result<bool> {
    let Some(relay) = &ws.get_ref().relay else {
        return Ok(false);
    };
    if relay.protocol < 2 {
        return Ok(false);
    }
    let Some(control) = &relay.cache else {
        return Ok(false);
    };
    control.update(idle).await?;
    Ok(true)
}
