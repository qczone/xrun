//! Network creation and explicit recovery of an existing authority.
use crate::error::ErrorCode;
use crate::{
    config::{self, Identity, NetworkIdentity},
    crypto, daemon,
    membership::Manager,
    net,
    protocol::RelayMessage,
    protocol::*,
};
use anyhow::{Context, Result, bail};
use std::time::Duration;

use super::links::{discover_relay_ca, endpoint, relay_pin};
use super::peers::{online_ids, states_for};
use super::transport::receive;
use super::{archive_legacy, cache, manager, observe};

pub(crate) async fn create(relay_link: &str, name: Option<String>) -> Result<Identity> {
    let endpoint = endpoint(relay_link)?;
    let mut root = None;
    let mut error = None;
    for address in &endpoint.addresses {
        match tokio::time::timeout(Duration::from_secs(5), async {
            let ca = discover_relay_ca(address, &endpoint.pin).await?;
            let mut ws = net::websocket_at(
                address,
                "/networks/probe/status",
                crypto::relay_tls_config(&ca)?,
            )
            .await?;
            // A challenge proves the random route; there is no member yet.
            if !matches!(receive(&mut ws).await?, RelayMessage::Challenge { .. }) {
                bail!(ErrorCode::InvalidMessage.error("expected a relay challenge"))
            }
            Ok::<_, anyhow::Error>(ca)
        })
        .await
        {
            Ok(Ok(ca)) => {
                root = Some(ca);
                break;
            }
            Ok(Err(e)) => error = Some(e),
            Err(_) => {
                error = Some(anyhow::anyhow!(
                    ErrorCode::ConnectTimeout.error("relay did not respond")
                ))
            }
        }
    }
    let relay_ca = root.ok_or_else(|| {
        error.unwrap_or_else(|| {
            anyhow::anyhow!(ErrorCode::ConnectFailed.error("relay is unavailable"))
        })
    })?;
    let _lock = daemon::instance_lock()?;
    let dir = config::device_dir()?;
    let existing = if dir.join("identity.toml").exists() {
        Some(Identity::load()?)
    } else {
        None
    };
    if let Some(old) = existing.as_ref().filter(|i| i.network.is_some()) {
        if name.as_ref().is_some_and(|name| name != &old.name) {
            bail!(ErrorCode::InvalidRequest.error("device names are immutable within a network"))
        }
        let manager = manager(old)?;
        let before = manager.roster()?;
        let next = if before.roster.relay_addresses == endpoint.addresses
            && relay_pin(&before.roster.relay_ca_pem)? == endpoint.pin
        {
            before.clone()
        } else {
            manager.set_relay(endpoint.addresses, relay_ca)?
        };
        observe(old, &next)?;
        // Deliver the signed move over encrypted peer sessions on the old relay.
        // Offline members can rejoin using a fresh invitation from this manager.
        if before.roster.relay_addresses != next.roster.relay_addresses
            && let Ok(ids) = online_ids(old, &before).await
        {
            let _ = states_for(old, &before, ids).await;
        }
        let mut id = old.clone();
        id.addresses = next.roster.relay_addresses;
        id.save()?;
        return Ok(id);
    }
    let name = name
        .or_else(|| existing.as_ref().map(|i| i.name.clone()))
        .unwrap_or_else(crate::client::user_name);
    let manager_dir = dir.join("manager");
    let (manager, member, key_pem, cert_pem) = if manager_dir.join("manager.db").exists() {
        let manager = Manager::open(&manager_dir)?;
        let roster = manager.roster()?;
        // Resume only an interrupted initial creation, never restore an older
        // authority over a network that has already admitted other members.
        if roster.roster.version != 1 || roster.roster.members.len() != 1 {
            bail!(
                ErrorCode::ManagerStateExists
                    .error("no matching local identity; refusing authority recovery")
            )
        }
        let key = std::fs::read_to_string(manager_dir.join("device.key.pending"))
            .context(ErrorCode::ManagerStateMissing.error("initial device key is missing"))?;
        let cert = std::fs::read_to_string(manager_dir.join("device.pem.pending"))?;
        let member = roster.member(&roster.roster.manager_id)?.clone();
        if member.key_fp != crypto::csr_key(&crypto::renew_device_request(&key)?)?
            || member.key_fp != crypto::peer_identity(&crypto::cert_der(&cert)?)?.1
        {
            bail!(
                ErrorCode::ManagerStateMismatch
                    .error("initial identity differs from the authority")
            )
        }
        if member.name != name
            || roster.roster.relay_addresses != endpoint.addresses
            || relay_pin(&roster.roster.relay_ca_pem)? != endpoint.pin
        {
            bail!(
                ErrorCode::ManagerStateMismatch
                    .error("retry initial creation with the same name and relay")
            )
        }
        (manager, member, key, cert)
    } else {
        Manager::create(&manager_dir, &name, endpoint.addresses, relay_ca)?
    };
    let roster = manager.roster()?;
    if existing.is_some() {
        archive_legacy(&dir)?;
    }
    cache()?.observe(&roster.roster.network_id, &roster)?;
    let id = Identity {
        device_id: member.device_id,
        name: member.name,
        addresses: roster.roster.relay_addresses.clone(),
        ca_pem: roster.ca_pem.clone(),
        cert_pem,
        key_pem,
        registration: Registration {
            inviter_id: None,
            allow_inviter: false,
        },
        network: Some(NetworkIdentity {
            network_id: roster.roster.network_id.clone(),
            manager_id: roster.roster.manager_id.clone(),
        }),
    };
    id.save()?;
    for file in ["device.key.pending", "device.pem.pending"] {
        std::fs::remove_file(manager_dir.join(file))?;
    }
    config::sync_parent(&manager_dir.join("manager.db"))?;
    Ok(id)
}
