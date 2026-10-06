//! Relay authentication and encrypted peer connections.
use crate::error::ErrorCode;
use crate::{
    config::Identity,
    crypto,
    membership::{Manager, SignedRoster},
    net::{self, Ws},
    protocol::{Proof, RelayMessage},
    secure,
};
use anyhow::{Result, bail};
use std::time::Duration;

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
        match tokio::time::timeout(Duration::from_secs(10), open_at(roster, &address, path)).await {
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
pub async fn authenticate(
    ws: &mut Ws,
    id: Option<&Identity>,
    network: &str,
    path: &str,
    root: Option<&Manager>,
) -> Result<()> {
    let RelayMessage::Challenge { nonce } = receive(ws).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected a relay challenge"))
    };
    let proof = id
        .map(|id| Proof::create(id, network, path, &nonce, root))
        .transpose()?;
    net::send(ws, &RelayMessage::Authenticate { proof }).await
}
pub(crate) async fn control(id: &Identity) -> Result<(Ws, String)> {
    let network = &authority(id)?.network_id;
    let path = format!("/networks/{network}/control");
    // The root-key signature lets the relay route pairing to the manager only.
    let root = if authority(id)?.manager_id == id.device_id {
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
    let RelayMessage::Connected { flow_control } = receive(&mut ws).await? else {
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
    let RelayMessage::Connected { flow_control } = receive(&mut outer).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected an encrypted tunnel"))
    };
    let (mut ws, cert) = tokio::time::timeout(
        Duration::from_secs(10),
        secure::client_with_flow(outer, id, target, flow_control),
    )
    .await??;
    tokio::time::timeout(
        Duration::from_secs(10),
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
    peer_session(id, target, &roster, secure::Purpose::Execute).await
}
