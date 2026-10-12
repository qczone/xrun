//! Relay authentication and encrypted peer connections.
use crate::error::ErrorCode;
use crate::{
    config::Identity,
    crypto,
    membership::{Manager, SignedRoster},
    net::{self, Ws},
    protocol::{CONNECT_TIMEOUT, Proof, RelayMessage},
    secure,
};
use anyhow::{Result, bail};

use super::{authority, cache, current, manager};

pub(crate) async fn receive(ws: &mut Ws) -> Result<RelayMessage> {
    match net::receive::<RelayMessage>(ws).await? {
        RelayMessage::Error { code, message } => {
            bail!(crate::error::CodedError::from_wire(code, message))
        }
        message => Ok(message),
    }
}
async fn open_at(roster: &SignedRoster, address: &str, path: &str) -> Result<Ws> {
    net::websocket_at(
        address,
        path,
        crypto::relay_tls_config(&roster.roster.relay_ca_pem)?,
    )
    .await
}
pub(super) async fn open_via(
    id: &Identity,
    roster: &SignedRoster,
    path: &str,
) -> Result<(Ws, String)> {
    let mut error = None;
    for address in net::ordered_addresses(
        &roster.roster.relay_addresses,
        &crypto::ca_spki_pin(&id.ca_pem)?,
    ) {
        match tokio::time::timeout(CONNECT_TIMEOUT, open_at(roster, &address, path)).await {
            Ok(Ok(ws)) => {
                net::remember(id, &address);
                return Ok((ws, address));
            }
            Ok(Err(e)) if net::explicit(&e) => return Err(e),
            Ok(Err(e)) => error = Some(e),
            Err(_) => {
                error = Some(anyhow::anyhow!(
                    ErrorCode::ConnectTimeout.error("relay did not respond")
                ))
            }
        }
    }
    Err(error
        .unwrap_or_else(|| anyhow::anyhow!(ErrorCode::ConnectFailed.error("no configured relay"))))
}
async fn open(id: &Identity, path: &str) -> Result<(Ws, String)> {
    open_via(id, &current(id)?, path).await
}
pub(crate) async fn latest_authority(roster: &SignedRoster) -> Result<Option<SignedRoster>> {
    for address in &roster.roster.relay_addresses {
        let connected = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            open_at(roster, address, "/networks/probe/status"),
        )
        .await;
        let Ok(Ok(mut ws)) = connected else {
            continue;
        };
        let message = tokio::time::timeout(CONNECT_TIMEOUT, receive(&mut ws)).await??;
        if let RelayMessage::Challenge {
            authority: Some(state),
            ..
        } = message
        {
            state.verify(&roster.roster.network_id)?;
            if crate::crypto::cert_der(&state.roster.ca_pem)?
                != crate::crypto::cert_der(&roster.ca_pem)?
            {
                bail!(ErrorCode::NetworkMismatch.error("relay restoration state has another root"));
            }
            // The archive can be newer than a relay, such as a staged handover.
            if state.roster.roster.version >= roster.roster.version
                && state.roster.roster.authority_generation >= roster.roster.authority_generation
            {
                roster.check_successor(&state.roster)?;
                return Ok(Some(state.roster));
            }
        }
    }
    Ok(None)
}
pub(crate) async fn authenticate(
    ws: &mut Ws,
    id: Option<&Identity>,
    network: &str,
    path: &str,
    root: Option<&Manager>,
) -> Result<()> {
    let known = if let Some(root) = root {
        Some(root.authority_state(
            i64::from(crate::config::DaemonConfig::load()?.membership_lease_minutes) * 60_000,
        )?)
    } else if id.is_some() {
        cache()?
            .authority(network)?
            .filter(|state| state.require_fresh(crate::protocol::now_ms()).is_ok())
    } else {
        None
    };
    let state = authenticate_with_state(ws, id, network, path, root, known).await?;
    if let (Some(id), Some(state)) = (id, state) {
        super::observe_authority(id, &state)?;
    }
    Ok(())
}
pub(crate) async fn authenticate_read_only(
    ws: &mut Ws,
    id: Option<&Identity>,
    network: &str,
    path: &str,
    root: Option<&Manager>,
) -> Result<Option<crate::membership::AuthorityState>> {
    authenticate_with_state(ws, id, network, path, root, None).await
}
async fn authenticate_with_state(
    ws: &mut Ws,
    id: Option<&Identity>,
    network: &str,
    path: &str,
    root: Option<&Manager>,
    known: Option<crate::membership::AuthorityState>,
) -> Result<Option<crate::membership::AuthorityState>> {
    let RelayMessage::Challenge {
        nonce,
        authority: state,
    } = receive(ws).await?
    else {
        bail!(ErrorCode::InvalidMessage.error("expected a relay challenge"))
    };
    if let Some(state) = &state {
        state.verify(network)?;
        if let Some(id) = id
            && crypto::cert_der(&state.roster.ca_pem)? != crypto::cert_der(&id.ca_pem)?
        {
            bail!(ErrorCode::NetworkMismatch.error("relay supplied another authority root"));
        }
    }
    let local_generation = root
        .map(|manager| manager.roster().map(|r| r.roster.authority_generation))
        .transpose()?;
    let root = root.filter(|_| {
        state.as_ref().is_none_or(|state| {
            local_generation
                .is_some_and(|generation| generation >= state.roster.roster.authority_generation)
                && (path.ends_with("/claim")
                    || local_generation.is_some_and(|generation| {
                        generation > state.roster.roster.authority_generation
                    })
                    || id.is_some_and(|id| state.roster.roster.manager_id == id.device_id))
        })
    });
    let generation = state
        .as_ref()
        .map_or(1, |state| state.roster.roster.authority_generation);
    let known = known.filter(|known| {
        state
            .as_ref()
            .is_none_or(|state| known.roster.roster.version >= state.roster.roster.version)
    });
    let proof = id
        .map(|id| {
            if known.is_some() {
                Proof::create_with_authority(id, network, path, &nonce, root, generation, known)
            } else {
                Proof::create_with_generation(id, network, path, &nonce, root, generation)
            }
        })
        .transpose()?;
    net::send(ws, &RelayMessage::Authenticate { proof }).await?;
    Ok(state)
}
pub(crate) async fn control(id: &Identity) -> Result<(Ws, String)> {
    let network = &authority(id)?.network_id;
    let path = format!("/networks/{network}/control");
    // The root-key signature lets the relay route pairing to the manager only.
    let root = if super::current(id)?.roster.manager_id == id.device_id {
        Some(manager(id)?)
    } else {
        None
    };
    let (mut ws, address) = open(id, &path).await?;
    authenticate(&mut ws, Some(id), network, &path, root.as_ref()).await?;
    Ok((ws, address))
}
pub(crate) async fn attach(
    id: &Identity,
    address: &str,
    generation: &str,
    sid: &str,
) -> Result<(Ws, bool)> {
    let roster = current(id)?;
    let path = format!(
        "/networks/{}/attach/{}/{generation}/{sid}",
        roster.roster.network_id, id.device_id
    );
    let mut ws = open_at(&roster, address, &path).await?;
    let (routed, reply) = data_route(ws, id, address, "target").await?;
    ws = routed;
    let RelayMessage::Connected { flow_control } = reply else {
        bail!(ErrorCode::InvalidMessage.error("expected attached tunnel"))
    };
    Ok((ws, flow_control))
}
pub(super) async fn peer_session(
    id: &Identity,
    target: &str,
    via: &SignedRoster,
    purpose: secure::Purpose,
) -> Result<(Ws, String, i64)> {
    let network = authority(id)?;
    let path = format!("/networks/{}/connect/{target}", network.network_id);
    let (mut outer, address) = open_via(id, via, &path).await?;
    authenticate(&mut outer, Some(id), &network.network_id, &path, None).await?;
    let (routed, reply) = data_route(outer, id, &address, "source").await?;
    outer = routed;
    let RelayMessage::Connected { flow_control } = reply else {
        bail!(ErrorCode::InvalidMessage.error("expected an encrypted tunnel"))
    };
    let (mut ws, cert) = tokio::time::timeout(
        CONNECT_TIMEOUT,
        secure::client_with_flow(outer, id, target, flow_control),
    )
    .await??;
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        secure::exchange_client(
            &mut ws,
            &cache()?,
            &network.network_id,
            &cert,
            target,
            &purpose,
        ),
    )
    .await??;
    current(id)?;
    let (_, peer) = x509_parser::parse_x509_certificate(&cert).map_err(|_| {
        anyhow::anyhow!(ErrorCode::InvalidCertificate.error("malformed target certificate"))
    })?;
    let expires = peer
        .validity()
        .not_after
        .timestamp()
        .min(crypto::certificate_expiry(&id.cert_pem)?)
        .min(crypto::certificate_expiry(&id.ca_pem)?);
    Ok((ws, address, expires))
}
pub(crate) async fn session(id: &Identity, target: &str) -> Result<(Ws, String)> {
    let (ws, address, _) = session_with_expiry(id, target).await?;
    Ok((ws, address))
}
pub(crate) async fn session_with_expiry(id: &Identity, target: &str) -> Result<(Ws, String, i64)> {
    let roster = current(id)?;
    if roster.member(target)?.revoked {
        bail!(ErrorCode::DeviceRevoked.error("target has been revoked"))
    }
    Box::pin(peer_session(id, target, &roster, secure::Purpose::Execute)).await
}

pub(crate) async fn channel(id: &Identity, target: &str) -> Result<(Ws, String, i64)> {
    let roster = current(id)?;
    if let Some(source) = crate::lan::selected().unwrap_or(None) {
        if crate::lan::candidate(id, target).is_none()
            && let Ok(state) = super::peers::peer_state(id, target, &roster).await
        {
            let _ = crate::lan::remember(id, target, state.lan);
        }
        if let Some(candidate) = crate::lan::candidate(id, target)
            && crate::lan::reachable(&source, &candidate)
        {
            match crate::lan::connect(id, target, &candidate).await {
                Ok((ws, expires)) => {
                    record_route(target, "direct")?;
                    return Ok((ws, "direct".into(), expires));
                }
                Err(error) => {
                    tracing::debug!(code=?crate::error::code(&error),"direct admission unavailable; trying relay before any business request");
                }
            }
        }
    }
    let result = Box::pin(peer_session(id, target, &roster, secure::Purpose::Channel)).await?;
    record_route(target, "relay")?;
    Ok(result)
}

pub(crate) async fn standby(id: &Identity, address: &str, generation: &str) -> Result<Ws> {
    let roster = current(id)?;
    let path = format!(
        "/networks/{}/standby/{}/{generation}",
        roster.roster.network_id, id.device_id
    );
    let mut ws = open_at(&roster, address, &path).await?;
    authenticate(&mut ws, Some(id), &roster.roster.network_id, &path, None).await?;
    let (routed, reply) = data_route(ws, id, address, "target").await?;
    ws = routed;
    if !matches!(reply, RelayMessage::StandbyReady) {
        bail!(ErrorCode::InvalidMessage.error("standby registration rejected"));
    }
    Ok(ws)
}

fn record_route(target: &str, kind: &str) -> Result<()> {
    let path = crate::config::device_dir()?.join("connection-routes.json");
    let mut routes: std::collections::BTreeMap<String, String> = if path.exists() {
        serde_json::from_slice(&std::fs::read(&path)?)?
    } else {
        Default::default()
    };
    if routes.len() >= 256 {
        routes.clear();
    }
    routes.insert(target.into(), kind.into());
    crate::config::atomic_private_write(&path, &serde_json::to_vec(&routes)?)
}

async fn data_route(
    ws: Ws,
    id: &Identity,
    address: &str,
    side: &str,
) -> Result<(Ws, RelayMessage)> {
    let roster = current(id)?;
    follow_data_route(
        ws,
        address,
        Some(&roster.roster.network_id),
        side,
        crypto::relay_tls_config(&roster.roster.relay_ca_pem)?,
    )
    .await
}

/// Delegate once, on the same relay and before any business or enrollment payload is sent.
pub(super) async fn follow_data_route(
    mut ws: Ws,
    address: &str,
    network: Option<&str>,
    side: &str,
    tls: std::sync::Arc<rustls::ClientConfig>,
) -> Result<(Ws, RelayMessage)> {
    let reply = receive(&mut ws).await?;
    if let RelayMessage::DataRoute { path } = reply {
        let parts: Vec<_> = path.split('/').collect();
        let fields =
            if path.starts_with("/.well-known/xrun/data/") && network.is_none() && parts.len() == 8
            {
                [parts[4], parts[5], parts[6], parts[7]]
            } else if parts.len() == 7
                && parts[0].is_empty()
                && parts[1] == "networks"
                && parts[3] == "data"
            {
                [parts[2], parts[4], parts[5], parts[6]]
            } else {
                bail!(ErrorCode::InvalidMessage.error("invalid delegated data route"));
            };
        let token =
            |value: &str| value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        if !(fields[0].len() == 56
            && fields[0].starts_with("net_")
            && fields[0][4..]
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || (b'2'..=b'7').contains(&byte)))
            || network.is_some_and(|expected| fields[0] != expected)
            || fields[2] != side
            || !token(fields[1])
            || !token(fields[3])
        {
            bail!(ErrorCode::InvalidMessage.error("invalid delegated data binding"));
        }
        ws = net::websocket_at(address, &path, tls).await?;
        let reply = receive(&mut ws).await?;
        if matches!(reply, RelayMessage::DataRoute { .. }) {
            bail!(ErrorCode::InvalidMessage.error("repeated data redirect"));
        }
        Ok((ws, reply))
    } else {
        Ok((ws, reply))
    }
}
