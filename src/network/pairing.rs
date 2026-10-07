//! Authority operations, pairing and certificate renewal.
use crate::error::ErrorCode;
use crate::{
    config::{self, Identity, NetworkIdentity, PendingIdentity},
    crypto, daemon,
    membership::{INVITATION_LIFETIME, Manager, Pairing},
    net::{self, Ws},
    protocol::RelayMessage,
    protocol::*,
    secure,
};
use anyhow::{Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

use super::links::{discover_relay_ca, invitation_link, parse_link};
use super::peers::peer_state;
use super::transport::{authenticate, receive};
use super::{archive_legacy, authority, cache, current, devices, observe, synchronize};

#[derive(Serialize, Deserialize)]
struct PairResponse {
    version: String,
    protocol: ProtocolRange,
    selected_protocol: u32,
    pairing: Pairing,
}
impl PairResponse {
    fn accept(self) -> Result<Pairing> {
        ProtocolRange::CURRENT.confirm(self.protocol, self.selected_protocol)?;
        Ok(self.pairing)
    }
}

pub fn manager(id: &Identity) -> Result<Manager> {
    if authority(id)?.manager_id != id.device_id {
        bail!(ErrorCode::NotManager.error("only the network manager can change membership"))
    }
    let manager = Manager::open(&config::device_dir()?.join("manager"))?;
    let r = manager.roster()?;
    if r.roster.network_id != authority(id)?.network_id || r.roster.manager_id != id.device_id {
        bail!(ErrorCode::ManagerStateMismatch.error("authority does not belong to this identity"))
    }
    current(id)?.check_successor(&r)?;
    Ok(manager)
}
pub async fn invite(id: &Identity, allow: bool) -> Result<serde_json::Value> {
    let manager = manager(id)?;
    let roster = manager.roster()?;
    observe(id, &roster)?;
    synchronize(&roster).await?;
    let token = manager.invite(allow)?;
    Ok(serde_json::json!({
        "link": invitation_link(&roster, &token)?,
        "allow": allow,
        "expires_in": INVITATION_LIFETIME.as_secs(),
    }))
}
pub(crate) async fn revoke(id: &Identity, selector: &str) -> Result<serde_json::Value> {
    let manager = manager(id)?;
    let roster = manager.revoke(selector)?;
    let target = roster.member(selector)?.device_id.clone();
    observe(id, &roster)?;
    // The revoked endpoint must learn its own revocation to close existing
    // sessions. Exchange delivers the signed record before rejecting its ID.
    // Offline targets cannot be confirmed; peers also enforce the update.
    let _ = peer_state(id, &target, &roster).await;
    let mut delivered = HashMap::new();
    let mut sync_error = None;
    match devices(id).await {
        Ok((_, acks)) => {
            for a in acks {
                delivered.insert(a.device_id, a.version);
            }
        }
        Err(error) => sync_error = Some(error.to_string()),
    }
    let undelivered: Vec<_> = roster
        .roster
        .members
        .iter()
        .filter(|m| {
            !m.revoked && m.device_id != id.device_id && !delivered.contains_key(&m.device_id)
        })
        .map(|m| m.device_id.clone())
        .collect();
    Ok(serde_json::json!({
        "device_id": target,
        "revoked": true,
        "roster_version": roster.roster.version,
        "undelivered": undelivered,
        "sync_error": sync_error,
    }))
}
async fn pairing_at(
    address: &str,
    relay_ca: &str,
    network: &str,
    root_pin: &str,
    manager: &str,
) -> Result<(Ws, String)> {
    let path = format!("/networks/{network}/connect/{manager}");
    let mut outer = net::websocket_at(address, &path, crypto::relay_tls_config(relay_ca)?).await?;
    authenticate(&mut outer, None, network, &path, None).await?;
    let RelayMessage::Connected { flow_control } = receive(&mut outer).await? else {
        bail!(ErrorCode::InvalidMessage.error("expected pairing tunnel"))
    };
    secure::pairing_client_with_flow(outer, root_pin, manager, flow_control).await
}
fn validate_pair(
    pair: &Pairing,
    network: &str,
    manager: &str,
    key: &str,
    root_pin: &str,
) -> Result<()> {
    pair.roster.verify(network)?;
    if pair.roster.roster.manager_id != manager
        || crypto::ca_spki_pin(&pair.roster.ca_pem)? != root_pin
    {
        bail!(ErrorCode::IdentityMismatch.error("pairing returned another authority"))
    }
    let fp = crypto::csr_key(&crypto::renew_device_request(key)?)?;
    if pair.member.key_fp != fp || pair.member != *pair.roster.member(&pair.member.device_id)? {
        bail!(ErrorCode::IdentityMismatch.error("pairing returned another member key"))
    }
    crypto::verify_member_certificate(&pair.cert_pem, &pair.roster.ca_pem, &pair.member.device_id)?;
    pair.roster.peer(
        &crypto::cert_der(&pair.cert_pem)?,
        Some(&pair.member.device_id),
    )?;
    pair.receipt.verify(&pair.roster, &pair.member.device_id)?;
    Ok(())
}
pub(crate) async fn join(link: &str, name: String) -> Result<Identity> {
    let invite = parse_link(link)?;
    let dir = config::device_dir()?;
    let existing = if dir.join("identity.toml").exists() {
        Some(Identity::load()?)
    } else {
        None
    };
    if let Some(old) = &existing
        && old.network.is_some()
    {
        if authority(old)?.network_id != invite.network {
            bail!(ErrorCode::NetworkMismatch.error("already joined to another network"))
        }
        current(old)?;
    }
    let _lock = if existing.as_ref().is_some_and(|id| id.network.is_some()) {
        None
    } else {
        Some(daemon::instance_lock()?)
    };
    let pending_path = dir.join("pending.toml");
    let key = if let Some(old) = &existing
        && old.network.is_some()
    {
        old.key_pem.clone()
    } else if pending_path.exists() {
        let p: PendingIdentity = config::read(&pending_path)?;
        if p.pin != invite.root_pin {
            bail!(ErrorCode::NetworkMismatch.error("pending pairing belongs to another network"))
        };
        p.key_pem
    } else {
        crypto::new_device_request()?.0
    };
    let mut error = None;
    for address in &net::ordered_addresses(&invite.addresses, &invite.root_pin) {
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let relay_ca = discover_relay_ca(address, &invite.relay_pin).await?;
            let (mut ws, root) = pairing_at(
                address,
                &relay_ca,
                &invite.network,
                &invite.root_pin,
                &invite.manager,
            )
            .await?;
            config::write(
                &pending_path,
                &PendingIdentity {
                    key_pem: key.clone(),
                    ca_pem: root,
                    pin: invite.root_pin.clone(),
                },
            )?;
            net::send(
                &mut ws,
                &PairRequest {
                    version: VERSION.into(),
                    protocol: ProtocolRange::CURRENT,
                    token: invite.token.clone(),
                    name: name.clone(),
                    csr_base64: STANDARD.encode(crypto::renew_device_request(&key)?),
                },
            )
            .await?;
            let value: serde_json::Value = net::receive(&mut ws).await?;
            if let Ok(Data::Error { code, message }) = serde_json::from_value(value.clone()) {
                bail!(crate::error::CodedError::from_wire(code, message))
            }
            let pair = serde_json::from_value::<PairResponse>(value)?.accept()?;
            validate_pair(
                &pair,
                &invite.network,
                &invite.manager,
                &key,
                &invite.root_pin,
            )?;
            if let Some(old) = &existing
                && old.network.is_some()
                && old.device_id != pair.member.device_id
            {
                bail!(ErrorCode::IdentityMismatch.error("pairing changed existing identity"))
            }
            let id = Identity {
                device_id: pair.member.device_id,
                name: pair.member.name,
                addresses: pair.roster.roster.relay_addresses.clone(),
                ca_pem: pair.roster.ca_pem.clone(),
                cert_pem: pair.cert_pem,
                key_pem: key.clone(),
                registration: Registration {
                    inviter_id: Some(invite.manager.clone()),
                    allow_inviter: pair.receipt.receipt.allow,
                },
                network: Some(NetworkIdentity {
                    network_id: invite.network.clone(),
                    manager_id: invite.manager.clone(),
                }),
            };
            if existing.as_ref().is_some_and(|i| i.network.is_none()) {
                archive_legacy(&dir)?;
            }
            cache()?.observe(&invite.network, &pair.roster)?;
            id.save()?;
            net::remember(&id, address);
            if id.registration.allow_inviter {
                config::update_daemon_config(&dir, |cfg| {
                    if !cfg.deny_from.contains(&invite.manager)
                        && !cfg.allow_from.contains(&invite.manager)
                    {
                        cfg.allow_from.push(invite.manager.clone());
                    }
                    Ok(())
                })?;
            }
            std::fs::remove_file(&pending_path)?;
            config::sync_parent(&pending_path)?;
            Ok::<_, anyhow::Error>(id)
        })
        .await
        .unwrap_or_else(|_| {
            Err(anyhow::anyhow!(
                ErrorCode::PairingTimeout.error("retry using the same invitation and pending key")
            ))
        });
        match result {
            Ok(id) => return Ok(id),
            Err(e) if net::explicit(&e) => return Err(e),
            Err(e) => error = Some(e),
        }
    }
    Err(error.unwrap_or_else(|| {
        anyhow::anyhow!(ErrorCode::ConnectFailed.error("no relay is reachable"))
    }))
}
pub(crate) async fn renew(id: &mut Identity) -> Result<()> {
    authority(id)?;
    // A manager whose key still exists can renew its own leaf without a relay.
    if !crypto::certificate_expiring(&id.cert_pem, 30)? {
        return Ok(());
    }
    let roster = current(id)?;
    let result = async {
        let pair = if roster.roster.manager_id == id.device_id {
            manager(id)?.pair("", &id.name, &crypto::renew_device_request(&id.key_pem)?)?
        } else {
            let mut answer = None;
            let mut error = None;
            for address in &roster.roster.relay_addresses {
                let attempt = tokio::time::timeout(Duration::from_secs(30), async {
                    let (mut ws, _) = pairing_at(
                        address,
                        &roster.roster.relay_ca_pem,
                        &roster.roster.network_id,
                        &crypto::ca_spki_pin(&id.ca_pem)?,
                        &roster.roster.manager_id,
                    )
                    .await?;
                    net::send(
                        &mut ws,
                        &PairRequest {
                            version: VERSION.into(),
                            protocol: ProtocolRange::CURRENT,
                            token: String::new(),
                            name: id.name.clone(),
                            csr_base64: STANDARD.encode(crypto::renew_device_request(&id.key_pem)?),
                        },
                    )
                    .await?;
                    let value: serde_json::Value = net::receive(&mut ws).await?;
                    if let Ok(Data::Error { code, message }) = serde_json::from_value(value.clone())
                    {
                        bail!(crate::error::CodedError::from_wire(code, message))
                    }
                    let pair = serde_json::from_value::<PairResponse>(value)?.accept()?;
                    validate_pair(
                        &pair,
                        &roster.roster.network_id,
                        &roster.roster.manager_id,
                        &id.key_pem,
                        &crypto::ca_spki_pin(&id.ca_pem)?,
                    )?;
                    Ok::<_, anyhow::Error>(pair)
                })
                .await
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!(
                        ErrorCode::RenewTimeout.error("manager did not complete renewal")
                    ))
                });
                match attempt {
                    Ok(pair) => {
                        answer = Some(pair);
                        break;
                    }
                    Err(e) => error = Some(e),
                }
            }
            answer.ok_or_else(|| {
                error.unwrap_or_else(|| {
                    anyhow::anyhow!(ErrorCode::ManagerOffline.error("manager is unavailable"))
                })
            })?
        };
        if pair.member.device_id != id.device_id {
            bail!(ErrorCode::IdentityMismatch.error("renewal changed the device ID"))
        }
        observe(id, &pair.roster)?;
        id.cert_pem = pair.cert_pem;
        id.save()?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => Ok(()),
        Err(e) if !crypto::certificate_expiring(&id.cert_pem, 0)? => {
            tracing::warn!(error=%e,"certificate renewal unavailable; using the still-valid member certificate");
            Ok(())
        }
        Err(e) => {
            Err(e.context(ErrorCode::CertificateExpired.error("renewal requires the manager")))
        }
    }
}
pub(crate) async fn serve_pair(id: &Identity, ws: &mut Ws) -> Result<()> {
    let manager = manager(id)?;
    let request: PairRequest =
        tokio::time::timeout(Duration::from_secs(10), net::receive(ws)).await??;
    let selected_protocol = ProtocolRange::CURRENT.negotiate(request.protocol)?;
    let pair = manager.pair(
        &request.token,
        &request.name,
        &STANDARD.decode(request.csr_base64)?,
    )?;
    observe(id, &pair.roster)?;
    if pair.receipt.receipt.allow && pair.member.device_id != id.device_id {
        pair.receipt.verify(&pair.roster, &pair.member.device_id)?;
        config::update_daemon_config(&config::device_dir()?, |cfg| {
            if !cfg.deny_from.contains(&pair.member.device_id)
                && !cfg.allow_from.contains(&pair.member.device_id)
            {
                cfg.allow_from.push(pair.member.device_id.clone());
            }
            Ok(())
        })?;
    }
    let roster = pair.roster.clone();
    net::send(
        ws,
        &PairResponse {
            version: VERSION.into(),
            protocol: ProtocolRange::CURRENT,
            selected_protocol,
            pairing: pair,
        },
    )
    .await?;
    // The durable pairing and grants above define success. Unrelated offline or
    // slow peers must not delay delivery of the certificate to its new owner.
    // This remains scoped to the manager's session; daemon shutdown cancels it.
    if let Err(error) = synchronize(&roster).await {
        tracing::warn!(%error,"membership committed but peer synchronization failed");
    }
    Ok(())
}
