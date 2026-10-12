//! Bounded device-pair channels; each local operation owns an independent encrypted stream.
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

const IDLE: Duration = Duration::from_secs(300);
const MAX_IDLE: usize = 3;
#[derive(Clone)]
struct Cached {
    channel: crate::multiplex::Channel,
    binding: String,
    expires: i64,
}
#[derive(Default)]
struct Pool {
    idle: Mutex<HashMap<String, Cached>>,
    opening: tokio::sync::Mutex<()>,
}
fn binding(id: &Identity) -> Result<String> {
    let roster = network::current(id)?;
    Ok(sha256(
        format!(
            "{}\0{}",
            ipc::identity_binding(id),
            serde_json::to_string(&(
                roster.roster.authority_generation,
                &roster.roster.relay_addresses,
                &roster.roster.relay_ca_pem
            ))?
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
            .map(|cached| {
                let cert = Duration::from_secs(
                    cached.expires.saturating_sub(now_ms() / 1000).max(0) as u64,
                );
                let idle = if cached.channel.active() == 0 {
                    IDLE.saturating_sub(cached.channel.idle_for())
                } else {
                    cert
                };
                Instant::now() + cert.min(idle).max(Duration::from_millis(100))
            })
            .min()
    }
    fn prune(&self) {
        self.idle.lock().unwrap().retain(|_, cached| {
            let valid = cached.channel.alive()
                && cached.expires > now_ms() / 1000
                && (cached.channel.active() > 0 || cached.channel.idle_for() < IDLE);
            if !valid {
                cached.channel.close();
            }
            valid
        });
    }
    fn cached(&self, target: &str, stamp: &str) -> Option<Cached> {
        self.prune();
        let mut entries = self.idle.lock().unwrap();
        if entries
            .get(target)
            .is_some_and(|cached| cached.binding != stamp)
            && let Some(cached) = entries.remove(target)
        {
            cached.channel.close();
        }
        entries.get(target).cloned()
    }
    async fn get(&self, id: &Identity, target: &str, stamp: &str) -> Result<(Cached, bool)> {
        if let Some(cached) = self.cached(target, stamp) {
            return Ok((cached, true));
        }
        let _opening = self.opening.lock().await;
        if let Some(cached) = self.cached(target, stamp) {
            return Ok((cached, true));
        }
        let (mut ws, _, expires) = network::channel(id, target).await?;
        match net::receive(&mut ws).await? {
            Data::ChannelReady {
                device_id,
                protocol,
            } if device_id == target => ProtocolRange::require(protocol)?,
            Data::Error { code, message } => {
                bail!(crate::error::CodedError::from_wire(code, message))
            }
            _ => bail!(
                ErrorCode::InvalidMessage
                    .error("target did not accept an encrypted operation channel")
            ),
        }
        let (channel, _inbound) = crate::multiplex::start(ws, yamux::Mode::Client);
        let cached = Cached {
            channel,
            binding: stamp.into(),
            expires,
        };
        self.insert(target, cached.clone());
        tracing::debug!(target, "opened operation session");
        Ok((cached, false))
    }
    fn insert(&self, target: &str, cached: Cached) {
        let mut entries = self.idle.lock().unwrap();
        let idle = entries
            .iter()
            .filter(|(_, c)| c.channel.active() == 0)
            .count();
        if idle >= MAX_IDLE
            && let Some(oldest) = entries
                .iter()
                .filter(|(_, c)| c.channel.active() == 0)
                .max_by_key(|(_, c)| c.channel.idle_for())
                .map(|(key, _)| key.clone())
            && let Some(old) = entries.remove(&oldest)
        {
            old.channel.close();
        }
        entries.insert(target.into(), cached);
    }
    async fn take(&self, id: &Identity, target: &str) -> Result<(Ws, Data, String, i64)> {
        let stamp = binding(id)?;
        if network::current(id)?.member(target)?.revoked {
            bail!(ErrorCode::DeviceRevoked.error("target has been revoked"));
        }
        for attempt in 0..2 {
            let (cached, reused) = self.get(id, target, &stamp).await?;
            let ready_stream = async {
                let mut ws = cached.channel.open().await?;
                net::send(
                    &mut ws,
                    &Data::SessionProbe {
                        roster_version: network::current(id)?.roster.version,
                    },
                )
                .await?;
                let (roster, lease) = match net::receive(&mut ws).await? {
                    Data::Membership { roster, lease } => (roster, lease),
                    Data::Error { code, message } => {
                        bail!(crate::error::CodedError::from_wire(code, message))
                    }
                    _ => bail!(
                        ErrorCode::InvalidMessage
                            .error("operation stream omitted current membership")
                    ),
                };
                network::observe_authority(
                    id,
                    &crate::membership::AuthorityState {
                        roster: roster.unwrap_or(network::current(id)?),
                        lease,
                    },
                )?;
                let value = net::receive::<Data>(&mut ws).await?;
                ready(&value, target)?;
                Ok::<_, anyhow::Error>((ws, value))
            };
            match tokio::time::timeout(Duration::from_secs(10), ready_stream).await {
                Ok(Ok((ws, value))) => {
                    if reused {
                        tracing::debug!(target, "reusing operation session");
                    }
                    return Ok((ws, value, stamp, cached.expires));
                }
                Ok(Err(error))
                    if attempt == 0
                        && (crate::error::is(&error, ErrorCode::ConnectionClosed)
                            || crate::error::is(&error, ErrorCode::SessionExpired)
                            || error
                                .downcast_ref::<tokio_tungstenite::tungstenite::Error>()
                                .is_some()) =>
                {
                    // This stream has not sent a business request. Only admission is retried.
                    cached.channel.close();
                    self.idle.lock().unwrap().remove(target);
                }
                Ok(Err(error)) => return Err(error),
                Err(error) => {
                    cached.channel.close();
                    return Err(error.into());
                }
            }
        }
        unreachable!()
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
fn local_manager(token: &str, expected: &str, version: &str, identity: &str) -> Result<Identity> {
    if token != expected {
        bail!(ErrorCode::Unauthenticated.error("invalid local endpoint token"));
    }
    if version != VERSION {
        bail!(ErrorCode::VersionMismatch.error("restart the daemon with this release"));
    }
    let id = Identity::load()?;
    if identity != ipc::identity_binding(&id) {
        bail!(ErrorCode::IdentityChanged.error("local identity changed"));
    }
    network::manager(&id)?;
    Ok(id)
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
    if let LocalRequest::Invite {
        token,
        version,
        identity,
        allow,
        replace,
    } = &opening
    {
        let id = local_manager(token, expected_token, version, identity)?;
        let invitation = control
            .invitations
            .create(&id, *allow, replace.as_deref())
            .await?;
        net::send(local, &LocalResponse::Invitation { invitation }).await?;
        return Ok(());
    }
    if let LocalRequest::CancelInvitation {
        token,
        version,
        identity,
        invitation_id,
    } = &opening
    {
        let id = local_manager(token, expected_token, version, identity)?;
        control.invitations.cancel(&id, invitation_id).await?;
        net::send(local, &LocalResponse::InvitationCancelled).await?;
        return Ok(());
    }
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
                    // Resumable uploads wait for window ACKs before End. Flush their frames now.
                    batch_body = matches!(request, Request::Exec { .. });
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
    let _ = (cacheable, stamp, expires);
    let mut remote = remote_tx.reunite(remote_rx)?;
    let _ = remote.close(None).await;
    drop(remote);
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
    #[tokio::test]
    async fn idle_pair_cache_is_bounded_and_expired_identity_channels_close() {
        let pool = Pool::default();
        let mut peers = vec![];
        for index in 0..=MAX_IDLE {
            let (client, server, _incoming) = crate::multiplex::tests::pair().await;
            peers.push(server);
            pool.insert(
                &format!("target{index}"),
                Cached {
                    channel: client,
                    binding: "fixture".into(),
                    expires: now_ms() / 1000 + 3600,
                },
            );
        }
        assert_eq!(pool.idle.lock().unwrap().len(), MAX_IDLE);
        assert!(!pool.idle.lock().unwrap().contains_key("target0"));
        for entry in pool.idle.lock().unwrap().values_mut() {
            entry.expires = now_ms() / 1000;
        }
        pool.prune();
        assert!(pool.idle.lock().unwrap().is_empty());
        for peer in peers {
            peer.close();
        }
    }
}
