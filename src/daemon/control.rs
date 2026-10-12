//! Relay control connection, reconnect and background roster synchronization.
use crate::error::ErrorCode;
use crate::{config::Identity, net, network, protocol::RelayMessage, protocol::*};
use anyhow::{Context, Result, bail};
use futures_util::{SinkExt, StreamExt};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::tungstenite::Message;

use super::Runtime;
use super::session::data_session;

pub(super) async fn control_reconnect(rt: Arc<Runtime>) -> Result<()> {
    let mut delay = 1u64;
    let mut invitations = rt.control.invitations.receiver();
    loop {
        let started = tokio::time::Instant::now();
        let result = control_once(rt.clone(), &mut invitations).await;
        rt.control.invitations.disconnected();
        record_connection(&rt.control, false);
        if started.elapsed() > HEARTBEAT_TIMEOUT {
            delay = 1;
        }
        if let Err(e) = result {
            tracing::warn!(error=%e,"daemon disconnected");
            if crate::error::is(&e, ErrorCode::IdentityChanged) {
                if crate::membership::backup::installation_retired(
                    &crate::config::device_dir()?,
                    &rt.id,
                    &network::current(&rt.id)?,
                )? {
                    // Launch agents and scheduled tasks only restart failures.
                    return Ok(());
                }
                return Err(e);
            }
        }
        let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]) * delay * 1000 / 1024;
        tokio::time::sleep(Duration::from_millis(delay * 1000 - jitter)).await;
        delay = (delay * 2).min(30);
    }
}
async fn control_once(
    rt: Arc<Runtime>,
    invitations: &mut tokio::sync::mpsc::Receiver<crate::pairing::Command>,
) -> Result<()> {
    let mut current = Identity::load()?;
    if current.device_id != rt.id.device_id || current.network != rt.id.network {
        bail!(ErrorCode::IdentityChanged.error("restart daemon after replacing its identity"));
    }
    let verified = network::current(&current)?;
    let obsolete =
        crate::membership::backup::authority_obsolete(&crate::config::device_dir()?, &verified)?;
    if obsolete && crate::membership::backup::holds_authority(&crate::config::device_dir()?)? {
        crate::client::network::retire_authority().await?;
        if verified.roster.manager_id == current.device_id {
            bail!(ErrorCode::IdentityChanged.error(
                "management was restored on another installation; this obsolete service has stopped"
            ));
        }
    }
    if verified.roster.manager_id == current.device_id {
        network::observe(&current, &network::manager(&current)?.roster()?)?;
    } else if crate::membership::backup::holds_authority(&crate::config::device_dir()?)? {
        crate::client::network::retire_authority().await?;
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
    let observed = network::current(&current)?;
    if (observed.roster.manager_id != current.device_id
        || crate::membership::backup::authority_obsolete(&crate::config::device_dir()?, &observed)?)
        && crate::membership::backup::holds_authority(&crate::config::device_dir()?)?
    {
        crate::client::network::retire_authority().await?;
        if observed.roster.manager_id == current.device_id {
            bail!(ErrorCode::IdentityChanged.error(
                "management was restored on another installation; this obsolete service has stopped"
            ));
        }
    }
    if let Some(state) = rt.members.authority(&rt.network_id)? {
        net::send(
            &mut ws,
            &RelayMessage::AuthorityAck {
                authority_generation: state.roster.roster.authority_generation,
                version: state.roster.roster.version,
                hash: state.roster.hash()?,
            },
        )
        .await?;
    }
    let protocol = ws
        .get_ref()
        .relay
        .as_ref()
        .map(|relay| relay.protocol)
        .unwrap_or(0);
    rt.control
        .invitations
        .connected(&address, &current.device_id, protocol);
    for registration in rt.control.invitations.registrations(&address) {
        net::send(&mut ws, &registration).await?;
    }
    record_connection(&rt.control, true);
    publish_authority(&rt, &mut ws).await?;
    let mut ping = tokio::time::interval(HEARTBEAT_INTERVAL);
    let mut prune = tokio::time::interval(LOG_PRUNE_INTERVAL);
    prune.tick().await; // Startup already pruned the store.
    let mut changes = rt.access.subscribe();
    let lease_minutes = rt.config()?.membership_lease_minutes;
    let mut leases = tokio::time::interval(std::time::Duration::from_secs(
        u64::from(lease_minutes) * 20,
    ));
    leases.tick().await;
    let standby = tokio::spawn(super::session::standby_loop(
        rt.clone(),
        address.clone(),
        generation.clone(),
    ));
    struct StopStandby(tokio::task::JoinHandle<Result<()>>);
    impl Drop for StopStandby {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _standby = StopStandby(standby);
    let mut last = tokio::time::Instant::now();
    loop {
        tokio::select! {
            Some(command) = invitations.recv() => {
                ProtocolRange::require(protocol)?;
                net::send(&mut ws, &command.message).await?;
            },
            _ = changes.changed() => {
                check_control_access(&rt, &address)?;
                if let Ok(config) = rt.config() {
                    publish_authority(&rt, &mut ws).await?;
                    leases.reset_after(Duration::from_secs(u64::from(config.membership_lease_minutes) * 20));
                }
            },
            _ = leases.tick() => publish_authority(&rt, &mut ws).await?,
            _ = ping.tick() => { rt.control.invitations.prune(); heartbeat(&mut ws, last).await?; },
            _ = prune.tick() => prune_logs(rt.clone()).await?,
            message = ws.next() => handle_control_message(&rt, &mut ws, message, &address, &generation, &mut last).await?,
        }
    }
}
fn record_connection(control: &crate::control::Control, connected: bool) {
    if let Err(error) = control.connected(connected) {
        // The runtime file is advisory. A failed status update must not stop
        // accepted jobs or prevent control reconnection; generation stays fixed.
        tracing::warn!(connected, error=%error, "unable to persist daemon connection state");
    }
}
async fn publish_authority(rt: &Runtime, ws: &mut net::Ws) -> Result<()> {
    let Ok(access) = rt.authorization() else {
        return Ok(());
    };
    let id = &access.identity;
    if network::current(id)?.roster.manager_id == id.device_id {
        let state = network::manager(id)?
            .authority_state(i64::from(access.config.membership_lease_minutes) * 60_000)?;
        rt.members.observe_authority(&rt.network_id, &state)?;
        net::send(ws, &RelayMessage::Authority { state }).await?;
    }
    Ok(())
}
const LOG_PRUNE_INTERVAL: Duration = Duration::from_secs(10 * 60);
fn check_control_access(rt: &Runtime, address: &str) -> Result<()> {
    // Session leases fail closed on an invalid snapshot. Keep the routing
    // connection long enough to report the original refusal and allow repair;
    // tearing it down first would discard those typed errors with its tunnels.
    let Ok(access) = rt.authorization() else {
        return Ok(());
    };
    if !access
        .roster
        .roster
        .relay_addresses
        .iter()
        .any(|value| value == address)
    {
        bail!(ErrorCode::RelayChanged.error("reconnect using the signed relay addresses"));
    }
    Ok(())
}
async fn heartbeat(ws: &mut net::Ws, last: tokio::time::Instant) -> Result<()> {
    if last.elapsed() > HEARTBEAT_TIMEOUT {
        bail!(ErrorCode::ControlTimeout.error("relay stopped responding"));
    }
    ws.send(Message::Ping(vec![].into())).await?;
    Ok(())
}
async fn prune_logs(rt: Arc<Runtime>) -> Result<()> {
    tokio::task::spawn_blocking(move || {
        rt.store.prune()?;
        super::transfers::expire_at(&crate::config::device_dir()?, &rt.store)
    })
    .await?
}
async fn handle_control_message(
    rt: &Arc<Runtime>,
    ws: &mut net::Ws,
    message: Option<std::result::Result<Message, tokio_tungstenite::tungstenite::Error>>,
    address: &str,
    generation: &str,
    last: &mut tokio::time::Instant,
) -> Result<()> {
    match message.context(ErrorCode::ConnectionClosed.error("control disconnected"))?? {
        Message::Pong(_) => *last = tokio::time::Instant::now(),
        Message::Ping(bytes) => {
            *last = tokio::time::Instant::now();
            ws.send(Message::Pong(bytes)).await?;
        }
        Message::Text(text) => {
            *last = tokio::time::Instant::now();
            match serde_json::from_str::<RelayMessage>(&text)? {
                RelayMessage::Authority { state } => {
                    network::observe_authority(&rt.id, &state)?;
                    net::send(
                        ws,
                        &RelayMessage::AuthorityAck {
                            authority_generation: state.roster.roster.authority_generation,
                            version: state.roster.roster.version,
                            hash: state.roster.hash()?,
                        },
                    )
                    .await?;
                }
                RelayMessage::AuthorityAck {
                    authority_generation,
                    version,
                    hash,
                } => {
                    let state = rt.members.load(&rt.network_id)?;
                    if authority_generation > state.roster.authority_generation
                        || version > state.roster.version
                        || (authority_generation == state.roster.authority_generation
                            && version == state.roster.version
                            && hash != state.hash()?)
                    {
                        bail!(
                            ErrorCode::InvalidAck
                                .error("relay acknowledged another membership state")
                        );
                    }
                }
                RelayMessage::PairingRegistered {
                    invitation_id,
                    error,
                } => rt.control.invitations.acknowledge(&invitation_id, error),
                RelayMessage::PairingIncoming {
                    session_id,
                    invitation_id,
                } => {
                    let runtime = rt.clone();
                    let address = address.to_string();
                    let generation = generation.to_string();
                    let permit = rt.sessions.clone().try_acquire_owned();
                    if let Ok(permit) = permit {
                        tokio::spawn(async move {
                            let _permit = permit;
                            let result = crate::network::code::serve(
                                &runtime.id,
                                &runtime.control.invitations,
                                &address,
                                &generation,
                                &session_id,
                                &invitation_id,
                            )
                            .await;
                            if let Err(error) = result {
                                tracing::debug!(code=?crate::error::code(&error), "short pairing ended");
                            }
                        });
                    } else {
                        net::send(
                            ws,
                            &RelayMessage::Reject {
                                session_id,
                                error: Box::new(Data::error(
                                    &ErrorCode::DeviceBusy
                                        .error("pairing capacity reached")
                                        .into(),
                                )),
                            },
                        )
                        .await?;
                    }
                }
                RelayMessage::Incoming { session_id } => {
                    let admission = rt.authorization().and_then(|_| {
                        rt.sessions
                            .clone()
                            .try_acquire_owned()
                            .context(ErrorCode::DeviceBusy.error("encrypted session limit reached"))
                    });
                    let permit = match admission {
                        Ok(permit) => permit,
                        Err(error) => {
                            net::send(
                                ws,
                                &RelayMessage::Reject {
                                    session_id,
                                    error: Box::new(Data::error(&error)),
                                },
                            )
                            .await?;
                            return Ok(());
                        }
                    };
                    let runtime = rt.clone();
                    let address = address.to_owned();
                    let generation = generation.to_owned();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(error) =
                            data_session(runtime, &address, &generation, &session_id).await
                        {
                            tracing::debug!(%error, "encrypted data session ended");
                        }
                    });
                }
                RelayMessage::Error { code, message } => {
                    bail!(crate::error::CodedError::from_wire(code, message))
                }
                _ => bail!(ErrorCode::InvalidMessage.error("unexpected control message")),
            }
        }
        _ => bail!(ErrorCode::ConnectionClosed.error("control disconnected")),
    }
    Ok(())
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
        maintain_membership(&mut cursor).await?;
    }
}
pub(crate) async fn maintain_membership(cursor: &mut usize) -> Result<()> {
    let mut id = Identity::load()?;
    // Healthy control sockets can outlive a certificate. Retry renewal here
    // as well, including when the manager was unavailable on the last pass.
    if let Err(error) = net::renew_identity(&mut id).await {
        tracing::warn!(%error, "background certificate renewal unavailable");
    }
    if let Err(error) = network::refresh_one(&id, cursor).await {
        tracing::debug!(%error, "peer membership synchronization unavailable");
    }
    Ok(())
}
