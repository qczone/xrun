//! Relay control connection, reconnect and background roster synchronization.
use crate::error::ErrorCode;
use crate::{config::Identity, net, network, protocol::*, relay::RelayMessage};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::tungstenite::Message;

use super::Runtime;
use super::session::data_session;

pub(super) async fn control_reconnect(rt: Arc<Runtime>) -> Result<()> {
    let mut delay = 1u64;
    loop {
        let started = tokio::time::Instant::now();
        let result = control_once(rt.clone()).await;
        rt.control.connected(false)?;
        if started.elapsed() > Duration::from_secs(45) {
            delay = 1;
        }
        if let Err(e) = result {
            tracing::warn!(error=%e,"daemon disconnected");
            if crate::error::is(&e, ErrorCode::IdentityChanged) {
                return Err(e);
            }
        }
        let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) * delay * 1000 / 1024;
        tokio::time::sleep(Duration::from_millis(delay * 1000 - jitter)).await;
        delay = (delay * 2).min(30);
    }
}
async fn control_once(rt: Arc<Runtime>) -> Result<()> {
    let mut current = Identity::load()?;
    if current.device_id != rt.id.device_id || current.network != rt.id.network {
        bail!(ErrorCode::IdentityChanged.error("restart daemon after replacing its identity"));
    }
    // Membership authority and synchronization belong to endpoints.
    if network::authority(&current)?.manager_id == current.device_id {
        network::observe(&current, &network::manager(&current)?.roster()?)?;
    }
    // Catch up once per connection from the manager, or from one random peer
    // while it is offline, so a relay restart does not make every device
    // contact every other device at once.
    let _ = network::refresh_one(&current, &mut sync_cursor()).await;
    net::renew_identity(&mut current).await?;
    let (mut ws, address) = network::control(&current).await?;
    let RelayMessage::HelloAck { generation } = network::receive(&mut ws).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected control acknowledgement"))
    };
    rt.control.connected(true)?;
    let mut ping = tokio::time::interval(Duration::from_secs(15));
    let mut last = tokio::time::Instant::now();
    let mut membership = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _=membership.tick()=>{
                if !network::current(&current)?.roster.relay_addresses.contains(&address) {
                    bail!(ErrorCode::RelayChanged.error("reconnect using the signed relay addresses"))
                }
            },
            _=ping.tick()=>{
                if last.elapsed()>Duration::from_secs(45){bail!(ErrorCode::ControlTimeout.error("relay stopped responding"))}
                ws.send(Message::Ping(vec![].into())).await?;
                rt.store.prune()?;
            },
            message=ws.next()=>{
                match message.context(ErrorCode::ConnectionClosed.error("control disconnected"))?? {
                    Message::Pong(_)=>last=tokio::time::Instant::now(),
                    Message::Ping(b)=>{last=tokio::time::Instant::now();ws.send(Message::Pong(b)).await?},
                    Message::Text(text)=>{
                        last=tokio::time::Instant::now();
                        match serde_json::from_str::<RelayMessage>(&text)? {
                            RelayMessage::Incoming{session_id,..}=>{
                                let Ok(permit)=rt.sessions.clone().try_acquire_owned() else {
                                    net::send(&mut ws,&RelayMessage::Reject{session_id,
                                        error:Box::new(Data::error(&anyhow::anyhow!(ErrorCode::DeviceBusy.error("encrypted session limit reached"))))}).await?;
                                    continue;
                                };
                                let rt=rt.clone();let address=address.clone();let generation=generation.clone();
                                tokio::spawn(async move{
                                    let _permit=permit;
                                    if let Err(error)=data_session(rt,&address,&generation,&session_id).await {
                                        tracing::debug!(%error,"encrypted data session ended");
                                    }
                                });
                            },
                            RelayMessage::Error{code,message}=>bail!(crate::error::CodedError::from_wire(code, message)),
                            _=>bail!(ErrorCode::InvalidMessage.error("unexpected control message")),
                        }
                    },
                    _=>bail!(ErrorCode::ConnectionClosed.error("control disconnected"))
                }
            }
        }
    }
}
fn sync_cursor() -> usize {
    usize::from(uuid::Uuid::new_v4().as_bytes()[0])
}
// The manager pushes membership changes, each reconnect catches up once, and
// every session exchanges signed rosters. This fallback only covers devices
// that missed all of those, so it runs rarely and with jitter: each pass costs
// relay connections and a mutual TLS handshake, usually with the manager.
const MEMBERSHIP_SYNC: Duration = Duration::from_secs(300);
pub(super) async fn membership_sync() -> Result<()> {
    let mut cursor = sync_cursor();
    loop {
        let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) * 60_000 / 256;
        tokio::time::sleep(MEMBERSHIP_SYNC + Duration::from_millis(jitter)).await;
        let id = Identity::load()?;
        if let Err(error) = network::refresh_one(&id, &mut cursor).await {
            tracing::debug!(%error, "peer membership synchronization unavailable");
        }
    }
}
