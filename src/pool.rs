//! Sequential sessions cached by the local daemon. Active requests exclusively
//! own their socket; the cache never multiplexes or retries submitted work.
use crate::error::ErrorCode;
use crate::{
    config::Identity,
    ipc::{self, LocalRequest, LocalResponse},
    net::{self, Ws},
    network,
    protocol::*,
};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinSet, time::Instant};
use tokio_tungstenite::tungstenite::Message;

// Measured 10/30/60-second bursts to three targets. Keep below the endpoint
// request idle timeout; this client's idle cache uses at most three of
// Cloudflare's eight session slots.
const IDLE: Duration = Duration::from_secs(75);
const MAX_IDLE: usize = 3;
struct Cached {
    ws: Ws,
    binding: String,
    since: Instant,
    expires: i64,
}
#[derive(Default)]
struct Pool {
    idle: Mutex<HashMap<String, Cached>>,
}
fn binding(id: &Identity) -> Result<String> {
    let roster = network::current(id)?;
    Ok(sha256(
        format!(
            "{}\0{}",
            ipc::identity_binding(id),
            serde_json::to_string(&roster)?
        )
        .as_bytes(),
    ))
}
fn ready(value: &Data, target: &str) -> Result<()> {
    match value {
        Data::Ready {
            protocol,
            selected_protocol,
            device_id,
            ..
        } if device_id == target => ProtocolRange::CURRENT.confirm(*protocol, *selected_protocol),
        Data::Error { code, message } => bail!(crate::error::CodedError::from_wire(code, message)),
        _ => bail!(ErrorCode::DeviceMismatch.error("invalid target acknowledgement")),
    }
}
impl Pool {
    fn next_expiry(&self) -> Option<Instant> {
        self.idle
            .lock()
            .unwrap()
            .values()
            .map(|c| {
                let remaining = c.expires.saturating_sub(now_ms() / 1000).max(0) as u64;
                (c.since + IDLE).min(Instant::now() + Duration::from_secs(remaining))
            })
            .min()
    }
    fn prune(&self) {
        self.idle
            .lock()
            .unwrap()
            .retain(|_, c| c.since.elapsed() < IDLE && c.expires > now_ms() / 1000);
    }
    async fn take(&self, id: &Identity, target: &str) -> Result<(Ws, Data, String, i64)> {
        let stamp = binding(id)?;
        let roster = network::current(id)?;
        if roster.member(target)?.revoked {
            bail!(ErrorCode::DeviceRevoked.error("target has been revoked"));
        }
        let cached = {
            let mut idle = self.idle.lock().unwrap();
            idle.retain(|device, cached| {
                let valid = cached.since.elapsed() < IDLE
                    && cached.expires > now_ms() / 1000
                    && cached.binding == stamp;
                if !valid {
                    tracing::debug!(
                        target = device,
                        idle_expired = cached.since.elapsed() >= IDLE,
                        certificate_expired = cached.expires <= now_ms() / 1000,
                        membership_changed = cached.binding != stamp,
                        "discarding cached operation session"
                    );
                }
                valid
            });
            idle.remove(target)
        };
        if let Some(mut cached) = cached {
            let probe = tokio::time::timeout(Duration::from_secs(5), async {
                net::send(
                    &mut cached.ws,
                    &Data::SessionProbe {
                        roster_version: roster.roster.version,
                    },
                )
                .await?;
                let value = net::receive::<Data>(&mut cached.ws).await?;
                ready(&value, target)?;
                Ok::<_, anyhow::Error>(value)
            })
            .await;
            match probe {
                Ok(Ok(value)) if cached.expires > now_ms() / 1000 => {
                    tracing::debug!(target, "reusing operation session");
                    return Ok((cached.ws, value, stamp, cached.expires));
                }
                result => {
                    tracing::debug!(target, ?result, "cached operation session probe failed");
                }
            }
            // The probe contains no operation. A stale/broken connection can
            // safely be replaced; requests are never replayed here.
        }
        let (mut ws, _, expires) = match network::session_with_expiry(id, target).await {
            Ok(session) => session,
            Err(e) if crate::error::is(&e, ErrorCode::SessionLimit) => {
                self.idle.lock().unwrap().clear();
                return Err(e);
            }
            Err(e) => return Err(e),
        };
        let value = net::receive::<Data>(&mut ws).await?;
        ready(&value, target)?;
        tracing::debug!(target, "opened operation session");
        Ok((ws, value, binding(id)?, expires))
    }
    fn put(&self, target: String, cached: Cached) {
        let mut idle = self.idle.lock().unwrap();
        idle.retain(|_, c| {
            c.since.elapsed() < IDLE && c.expires > now_ms() / 1000 && c.binding == cached.binding
        });
        if idle.len() >= MAX_IDLE
            && !idle.contains_key(&target)
            && let Some(oldest) = idle
                .iter()
                .min_by_key(|(_, c)| c.since)
                .map(|(key, _)| key.clone())
        {
            idle.remove(&oldest);
        }
        idle.insert(target, cached);
    }
}
pub(crate) async fn run(
    mut listener: ipc::Listener,
    control: Arc<crate::control::Control>,
) -> Result<()> {
    let pool = Arc::new(Pool::default());
    let permits = Arc::new(Semaphore::new(32));
    let mut tasks = JoinSet::new();
    loop {
        let expiry = pool.next_expiry();
        let expires = async {
            if let Some(deadline) = expiry {
                tokio::time::sleep_until(deadline).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            accepted = listener.accept() => {
                let mut ws = match accepted { Ok(ws) => ws, Err(e) => { tracing::warn!(%e, "local connection failed"); continue; } };
                // Reserve room for authenticated stop requests even when all
                // operation slots are occupied by long-running streams.
                if tasks.len() >= 64 { continue; }
                let permits = permits.clone();
                let pool = pool.clone();
                let token = listener.token();
                let control = control.clone();
                tasks.spawn(async move {
                    if let Err(e) = handle(&mut ws, pool, &token, &control, permits).await {
                        let _ = tokio::time::timeout(Duration::from_secs(1), net::send(&mut ws, &Data::error(&e))).await;
                        tracing::debug!(%e, "local operation ended");
                    }
                });
            }
            _ = expires => pool.prune(),
            _ = tasks.join_next(), if !tasks.is_empty() => {},
        }
    }
}
async fn handle(
    local: &mut Ws,
    pool: Arc<Pool>,
    expected_token: &str,
    control: &crate::control::Control,
    permits: Arc<Semaphore>,
) -> Result<()> {
    let opening: LocalRequest =
        tokio::time::timeout(Duration::from_secs(5), net::receive(local)).await??;
    if let LocalRequest::Stop { token, generation } = &opening {
        if token != expected_token {
            bail!(ErrorCode::Unauthenticated.error("invalid local endpoint token"));
        }
        control.check_generation(generation)?;
        net::send(local, &LocalResponse::Stopped).await?;
        control.request_stop(generation)?;
        return Ok(());
    }
    if let LocalRequest::ReloadAccess { token } = &opening {
        if token != expected_token {
            bail!(ErrorCode::Unauthenticated.error("invalid local endpoint token"));
        }
        control.reload_access().await?;
        net::send(local, &LocalResponse::AccessReloaded).await?;
        return Ok(());
    }
    let LocalRequest::Open {
        token,
        version,
        identity,
        target,
    } = opening
    else {
        bail!(ErrorCode::InvalidMessage.error("expected local open"));
    };
    if token != expected_token {
        bail!(ErrorCode::Unauthenticated.error("invalid local endpoint token"));
    }
    if version != VERSION {
        bail!(ErrorCode::VersionMismatch.error("local release differs"));
    }
    let _permit = permits
        .try_acquire_owned()
        .context(ErrorCode::DeviceBusy.error("local operation capacity reached"))?;
    let id = Identity::load()?;
    if identity != ipc::identity_binding(&id) {
        bail!(ErrorCode::IdentityMismatch.error("local identity changed"));
    }
    let (remote, ready, stamp, expires) =
        tokio::time::timeout(Duration::from_secs(30), pool.take(&id, &target)).await??;
    net::send(local, &ready).await?;
    let (mut remote_tx, mut remote_rx) = remote.split();
    let (mut local_tx, mut local_rx) = local.split();
    let complete = AtomicBool::new(false);
    let complete_signal = tokio::sync::Notify::new();
    let upload = async {
        let mut requested = false;
        let mut cacheable = false;
        let mut batch_body = false;
        loop {
            let message = if complete.load(Ordering::Acquire) {
                tokio::time::timeout(Duration::from_secs(5), local_rx.next()).await?
            } else {
                tokio::select! {
                    message = local_rx.next() => message,
                    _ = complete_signal.notified() => continue,
                }
            }
            .context(ErrorCode::ConnectionClosed.error("local client disconnected"))??;
            if let Message::Text(text) = &message {
                if matches!(
                    serde_json::from_str::<LocalRequest>(text),
                    Ok(LocalRequest::Release)
                ) {
                    if !complete.load(Ordering::Acquire) {
                        bail!(ErrorCode::InvalidMessage.error("release before completion"));
                    }
                    return Ok::<_, anyhow::Error>(cacheable);
                }
                if let Ok(Data::Request { request }) = serde_json::from_str(text) {
                    if requested {
                        bail!(
                            ErrorCode::InvalidMessage.error("one operation per local connection")
                        );
                    }
                    requested = true;
                    batch_body = matches!(request, Request::Push { .. } | Request::Exec { .. });
                    cacheable = !matches!(
                        request,
                        Request::Forward { .. } | Request::StreamExec { .. }
                    );
                } else if !requested {
                    bail!(ErrorCode::InvalidMessage.error("expected operation"));
                }
            } else if !requested || matches!(message, Message::Close(_)) {
                bail!(ErrorCode::ConnectionClosed.error("incomplete local operation"));
            }
            if batch_body && matches!(message, Message::Binary(_)) {
                remote_tx.feed(message).await?;
            } else {
                remote_tx.send(message).await?;
            }
        }
    };
    let download = async {
        while let Some(message) = remote_rx.next().await {
            let message = message?;
            let done = matches!(&message, Message::Text(text) if matches!(serde_json::from_str::<Data>(text), Ok(Data::Complete)));
            if done {
                complete.store(true, Ordering::Release);
            }
            local_tx.send(message).await?;
            if done {
                complete_signal.notify_one();
                return Ok::<_, anyhow::Error>(());
            }
        }
        bail!(ErrorCode::ConnectionClosed.error("target disconnected"));
    };
    let (cacheable, ()) = tokio::try_join!(upload, download)?;
    // Both pumps are finished and the CLI consumed Complete before Release.
    let remote = remote_tx.reunite(remote_rx)?;
    if cacheable && binding(&Identity::load()?).is_ok_and(|current| current == stamp) {
        pool.put(
            target,
            Cached {
                ws: remote,
                binding: stamp,
                since: Instant::now(),
                expires,
            },
        );
    }
    local_tx
        .send(Message::Text(
            serde_json::to_string(&LocalResponse::Released)?.into(),
        ))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn entry(since: Instant, expires: i64) -> Cached {
        let (io, _peer) = tokio::io::duplex(1024);
        let ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
            Box::new(io) as net::Io,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        Cached {
            ws,
            binding: "identity".into(),
            since,
            expires,
        }
    }
    #[tokio::test]
    async fn cache_caps_idle_sockets_and_expires_certificates_and_idle_entries() {
        let pool = Pool::default();
        let expires = now_ms() / 1000 + 300;
        for index in 0..=MAX_IDLE {
            pool.put(
                format!("device{index}"),
                entry(
                    Instant::now() - Duration::from_secs((MAX_IDLE - index + 1) as u64),
                    expires,
                )
                .await,
            );
        }
        assert_eq!(pool.idle.lock().unwrap().len(), MAX_IDLE);
        assert!(!pool.idle.lock().unwrap().contains_key("device0"));
        {
            let mut idle = pool.idle.lock().unwrap();
            idle.get_mut("device1").unwrap().expires = now_ms() / 1000;
            for index in 2..=MAX_IDLE {
                idle.get_mut(&format!("device{index}")).unwrap().since = Instant::now() - IDLE;
            }
        }
        pool.prune();
        assert!(pool.idle.lock().unwrap().is_empty());
        assert!(pool.next_expiry().is_none());
    }
}
