//! Peer presence, state queries and signed membership synchronization.
use crate::error::ErrorCode;
use crate::{
    config::{self, Identity},
    membership::{Member, ReceiptAck, SignedRoster},
    net,
    protocol::RelayMessage,
    protocol::*,
    secure,
};
use anyhow::{Context, Result, bail};
use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

use super::transport::{authenticate, open_via, peer_session, receive};
use super::{authority, current, observe};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerState {
    pub device: Device,
    pub ack: ReceiptAck,
}
pub(crate) fn local_device(id: &Identity, cwd: String) -> Result<Device> {
    let roster = current(id)?;
    Ok(Device {
        device_id: id.device_id.clone(),
        name: id.name.clone(),
        online: true,
        admin: id.device_id == roster.roster.manager_id,
        revoked: false,
        os: Some(std::env::consts::OS.into()),
        arch: Some(std::env::consts::ARCH.into()),
        version: Some(VERSION.into()),
        hostname: std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .ok(),
        execution_user: std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok(),
        default_cwd: Some(cwd),
        last_seen: Some(now_ms()),
    })
}
pub(super) async fn peer_state(
    id: &Identity,
    target: &str,
    via: &SignedRoster,
) -> Result<PeerState> {
    // Public relays add edge and inter-device round trips to the full handshake.
    let timeout = Duration::from_secs(if via.roster.relay_ca_pem.is_empty() {
        20
    } else {
        5
    });
    tokio::time::timeout(timeout, async {
        let (mut ws, _, _) = peer_session(id, target, via, secure::Purpose::State).await?;
        let value: serde_json::Value = net::receive(&mut ws).await?;
        if let Ok(Data::Error { code, message }) = serde_json::from_value(value.clone()) {
            bail!(crate::error::CodedError::from_wire(code, message))
        }
        let state: PeerState = serde_json::from_value(value)?;
        if state.device.device_id != target || state.ack.device_id != target {
            bail!(ErrorCode::IdentityMismatch.error("state belongs to another device"))
        }
        state.ack.verify(&current(id)?)?;
        Ok::<_, anyhow::Error>(state)
    })
    .await
    .context(ErrorCode::ConnectTimeout.error("peer state request timed out"))?
}
pub(super) async fn online_ids(id: &Identity, via: &SignedRoster) -> Result<Vec<String>> {
    let network = &authority(id)?.network_id;
    let path = format!("/networks/{network}/status");
    let (mut ws, _) = open_via(id, via, &path).await?;
    authenticate(&mut ws, Some(id), network, &path, None).await?;
    let RelayMessage::Status { mut devices } = receive(&mut ws).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected relay routes"))
    };
    if devices.len() > 256 {
        bail!(ErrorCode::InvalidMessage.error("invalid relay routes"))
    }
    devices.retain(|d| crate::membership::device_name(d).is_ok());
    devices.sort();
    devices.dedup();
    Ok(devices)
}
pub(super) async fn states_for(
    id: &Identity,
    via: &SignedRoster,
    ids: Vec<String>,
) -> Result<Vec<PeerState>> {
    let answers = stream::iter(
        ids.into_iter()
            .filter(|target| {
                target != &id.device_id && via.member(target).is_ok_and(|member| !member.revoked)
            })
            .map(|target| async move {
                let result = peer_state(id, &target, via).await;
                (target, result)
            }),
    )
    .buffer_unordered(8)
    .collect::<Vec<_>>()
    .await;
    // Routing hints never establish membership or online identity.
    let current = current(id)?;
    let mut states = Vec::new();
    for (target, answer) in answers {
        match answer {
            Ok(state) => states.push(state),
            Err(error)
                if crate::error::is(&error, ErrorCode::DeviceRevoked)
                    && current.member(&target).is_ok_and(|m| !m.revoked) =>
            {
                return Err(error);
            }
            Err(error) => tracing::debug!(%target, %error, "peer state request unavailable"),
        }
    }
    Ok(states)
}
fn listed_device(roster: &SignedRoster, member: &Member, info: Option<&Device>) -> Device {
    let info = info.filter(|_| !member.revoked);
    Device {
        device_id: member.device_id.clone(),
        name: member.name.clone(),
        online: info.is_some(),
        admin: member.device_id == roster.roster.manager_id,
        revoked: member.revoked,
        os: info.and_then(|d| d.os.clone()),
        arch: info.and_then(|d| d.arch.clone()),
        version: info.and_then(|d| d.version.clone()),
        hostname: info.and_then(|d| d.hostname.clone()),
        execution_user: info.and_then(|d| d.execution_user.clone()),
        default_cwd: info.and_then(|d| d.default_cwd.clone()),
        last_seen: info.and_then(|d| d.last_seen),
    }
}

/// Relay presence is a routing hint, not proof that an endpoint can execute.
pub(crate) async fn connected_devices(id: &Identity) -> Result<Vec<Device>> {
    let ids = online_ids(id, &current(id)?).await?;
    let roster = current(id)?;
    Ok(roster
        .roster
        .members
        .iter()
        .map(|member| {
            let mut device = listed_device(&roster, member, None);
            device.online = !member.revoked && ids.contains(&member.device_id);
            device
        })
        .collect())
}

pub(crate) async fn device(id: &Identity, selector: &str) -> Result<Device> {
    let via = current(id)?;
    let member = via.member(selector)?;
    if member.revoked {
        bail!(ErrorCode::DeviceRevoked.error("target has been revoked"))
    }
    let state = peer_state(id, &member.device_id, &via).await;
    // The peer exchange may have delivered a newer signed roster.
    let roster = current(id)?;
    let member = roster.member(&member.device_id)?;
    if member.revoked {
        bail!(ErrorCode::DeviceRevoked.error("target has been revoked"))
    }
    let info = match state {
        Ok(state) => Some(state.device),
        Err(error) if crate::error::is(&error, ErrorCode::DeviceRevoked) => return Err(error),
        Err(error) => {
            tracing::debug!(target = %member.device_id, %error, "peer state request unavailable");
            None
        }
    };
    Ok(listed_device(&roster, member, info.as_ref()))
}

pub(crate) async fn devices(id: &Identity) -> Result<(Vec<Device>, Vec<ReceiptAck>)> {
    let via = current(id)?;
    let ids = online_ids(id, &via).await?;
    let mut states = states_for(id, &via, ids.clone()).await?;
    if ids.contains(&id.device_id)
        && config::instance_running(&config::device_dir()?.join("daemon.lock"))?
    {
        let cfg = config::DaemonConfig::load()?;
        let cwd = cfg
            .default_cwd
            .unwrap_or(config::home_dir()?)
            .to_string_lossy()
            .into();
        states.push(PeerState {
            device: local_device(id, cwd)?,
            ack: ReceiptAck::create(id, &current(id)?)?,
        });
    }
    let roster = current(id)?;
    let acks = states
        .iter()
        .map(|s| s.ack.clone())
        .filter(|a| a.verify(&roster).is_ok())
        .collect();
    let metadata: HashMap<_, _> = states
        .into_iter()
        .map(|s| (s.device.device_id.clone(), s.device))
        .collect();
    let devices = roster
        .roster
        .members
        .iter()
        .map(|member| {
            let info = metadata.get(&member.device_id).filter(|_| !member.revoked);
            listed_device(&roster, member, info)
        })
        .collect();
    Ok((devices, acks))
}
pub(crate) async fn refresh(id: &Identity) -> Result<()> {
    let via = current(id)?;
    states_for(id, &via, online_ids(id, &via).await?).await?;
    Ok(())
}
pub(crate) async fn refresh_one(id: &Identity, cursor: &mut usize) -> Result<()> {
    let via = current(id)?;
    let peers: Vec<_> = online_ids(id, &via)
        .await?
        .into_iter()
        .filter(|target| target != &id.device_id && via.member(target).is_ok_and(|m| !m.revoked))
        .collect();
    if peers.is_empty() {
        return Ok(());
    }
    let fallback = &peers[*cursor % peers.len()];
    *cursor = cursor.wrapping_add(1);
    let manager = &via.roster.manager_id;
    if manager != &id.device_id
        && peers.contains(manager)
        && peer_state(id, manager, &via).await.is_ok()
    {
        return Ok(());
    }
    peer_state(id, fallback, &via).await?;
    Ok(())
}
pub(crate) async fn synchronize(roster: &SignedRoster) -> Result<()> {
    let id = Identity::load()?;
    if authority(&id)?.network_id != roster.roster.network_id {
        bail!(ErrorCode::NetworkMismatch.error("local identity differs from the signed roster"))
    }
    observe(&id, roster)?;
    refresh(&id).await
}
