use crate::{
    config::{self, Identity, NetworkIdentity, PendingIdentity},
    crypto, daemon,
    membership::{Manager, Pairing, ReceiptAck, RosterCache, SignedRoster},
    net::{self, Ws},
    protocol::*,
    relay::RelayMessage,
    secure,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{StreamExt, stream};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

pub fn authority(id: &Identity) -> Result<&NetworkIdentity> {
    let network = id.network.as_ref().context(
        "MIGRATION_REQUIRED: stop old services, then create or join an end-to-end network",
    )?;
    if network.network_id != format!("net_{}", crypto::ca_spki_pin(&id.ca_pem)?) {
        bail!("NETWORK_MISMATCH: identity root differs from its network ID")
    }
    Ok(network)
}
pub fn cache() -> Result<RosterCache> {
    RosterCache::open(&config::device_dir()?.join("roster.db"))
}
pub fn current(id: &Identity) -> Result<SignedRoster> {
    let network = authority(id)?;
    let path = config::device_dir()?.join("roster.db");
    if !path.exists() {
        bail!("MEMBER_STATE_MISSING: refusing to discard the highest known roster version")
    }
    let value = RosterCache::open(&path)?.load(&network.network_id)?;
    if value.roster.manager_id != network.manager_id {
        bail!("IDENTITY_MISMATCH: network manager differs from the identity")
    }
    let self_member = value.member(&id.device_id)?;
    if self_member.key_fp != crypto::peer_identity(&crypto::cert_der(&id.cert_pem)?)?.1 {
        bail!("IDENTITY_MISMATCH: local key differs from its membership")
    }
    if self_member.revoked {
        bail!("DEVICE_REVOKED: local identity has been revoked")
    }
    Ok(value)
}
pub fn observe(id: &Identity, value: &SignedRoster) -> Result<()> {
    let network = authority(id)?;
    if value.roster.manager_id != network.manager_id {
        bail!("IDENTITY_MISMATCH: signed roster names another manager")
    }
    match cache()?.observe(&network.network_id, value) {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().starts_with("ROSTER_ROLLBACK") => Ok(()),
        Err(e) => Err(e),
    }
}
pub async fn receive(ws: &mut Ws) -> Result<RelayMessage> {
    match net::receive::<RelayMessage>(ws).await? {
        RelayMessage::Error { code, message } => bail!("{code}: {message}"),
        message => Ok(message),
    }
}
async fn open_at(roster: &SignedRoster, address: &str, path: &str) -> Result<Ws> {
    net::websocket_at(
        address,
        path,
        crypto::anonymous_tls_config(&roster.roster.relay_ca_pem)?,
    )
    .await
}
async fn open_via(id: &Identity, roster: &SignedRoster, path: &str) -> Result<(Ws, String)> {
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
            Err(_) => error = Some(anyhow::anyhow!("CONNECT_TIMEOUT: relay did not respond")),
        }
    }
    Err(error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: no configured relay")))
}
pub async fn open(id: &Identity, path: &str) -> Result<(Ws, String)> {
    open_via(id, &current(id)?, path).await
}
pub async fn control(id: &Identity) -> Result<(Ws, String)> {
    open(
        id,
        &format!("/networks/{}/control", authority(id)?.network_id),
    )
    .await
}
pub async fn attach(id: &Identity, address: &str, generation: &str, sid: &str) -> Result<Ws> {
    let roster = current(id)?;
    let path = format!(
        "/networks/{}/attach/{}/{generation}/{sid}",
        roster.roster.network_id, id.device_id
    );
    let mut ws = open_at(&roster, address, &path).await?;
    if !matches!(receive(&mut ws).await?, RelayMessage::Connected) {
        bail!("INVALID_MESSAGE: expected attached tunnel")
    }
    Ok(ws)
}
async fn peer_session(
    id: &Identity,
    target: &str,
    via: &SignedRoster,
    purpose: secure::Purpose,
) -> Result<(Ws, String)> {
    let network = authority(id)?;
    let path = format!("/networks/{}/connect/{target}", network.network_id);
    let (mut outer, address) = open_via(id, via, &path).await?;
    if !matches!(receive(&mut outer).await?, RelayMessage::Connected) {
        bail!("INVALID_MESSAGE: expected an encrypted tunnel")
    }
    let (mut ws, cert) =
        tokio::time::timeout(Duration::from_secs(10), secure::client(outer, id, target)).await??;
    tokio::time::timeout(
        Duration::from_secs(10),
        secure::exchange_client(&mut ws, &cache()?, &network.network_id, &cert, target),
    )
    .await??;
    current(id)?;
    net::send(&mut ws, &purpose).await?;
    Ok((ws, address))
}
pub async fn session(id: &Identity, target: &str) -> Result<(Ws, String)> {
    let roster = current(id)?;
    if roster.member(target)?.revoked {
        bail!("DEVICE_REVOKED: target has been revoked")
    }
    peer_session(id, target, &roster, secure::Purpose::Execute).await
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerState {
    pub device: Device,
    pub ack: ReceiptAck,
}
pub fn local_device(id: &Identity, cwd: String) -> Result<Device> {
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
async fn peer_state(id: &Identity, target: &str, via: &SignedRoster) -> Result<PeerState> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let (mut ws, _) = peer_session(id, target, via, secure::Purpose::State).await?;
        let value: serde_json::Value = net::receive(&mut ws).await?;
        if let Ok(Data::Error { code, message }) = serde_json::from_value(value.clone()) {
            bail!("{code}: {message}")
        }
        let state: PeerState = serde_json::from_value(value)?;
        if state.device.device_id != target || state.ack.device_id != target {
            bail!("IDENTITY_MISMATCH: state belongs to another device")
        }
        state.ack.verify(&current(id)?)?;
        Ok::<_, anyhow::Error>(state)
    })
    .await
    .context("CONNECT_TIMEOUT: peer state request timed out")?
}
async fn online_ids(id: &Identity, via: &SignedRoster) -> Result<Vec<String>> {
    let path = format!("/networks/{}/status", authority(id)?.network_id);
    let (mut ws, _) = open_via(id, via, &path).await?;
    let RelayMessage::Status { mut devices } = receive(&mut ws).await? else {
        bail!("INVALID_MESSAGE: expected relay routes")
    };
    if devices.len() > 256 {
        bail!("INVALID_MESSAGE: invalid relay routes")
    }
    devices.retain(|d| crate::membership::device_name(d).is_ok());
    devices.sort();
    devices.dedup();
    Ok(devices)
}
async fn states_for(id: &Identity, via: &SignedRoster, ids: Vec<String>) -> Result<Vec<PeerState>> {
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
                if error.to_string().starts_with("DEVICE_REVOKED")
                    && current.member(&target).is_ok_and(|m| !m.revoked) =>
            {
                return Err(error);
            }
            Err(_) => {}
        }
    }
    Ok(states)
}
pub async fn devices(id: &Identity) -> Result<(Vec<Device>, Vec<ReceiptAck>)> {
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
        })
        .collect();
    Ok((devices, acks))
}
pub async fn refresh(id: &Identity) -> Result<()> {
    let via = current(id)?;
    states_for(id, &via, online_ids(id, &via).await?).await?;
    Ok(())
}
pub async fn refresh_one(id: &Identity, cursor: &mut usize) -> Result<()> {
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
pub async fn synchronize(roster: &SignedRoster) -> Result<()> {
    let id = Identity::load()?;
    if authority(&id)?.network_id != roster.roster.network_id {
        bail!("NETWORK_MISMATCH: local identity differs from the signed roster")
    }
    observe(&id, roster)?;
    refresh(&id).await
}
pub fn manager(id: &Identity) -> Result<Manager> {
    if authority(id)?.manager_id != id.device_id {
        bail!("NOT_MANAGER: only the network manager can change membership")
    }
    let manager = Manager::open(&config::device_dir()?.join("manager"))?;
    let r = manager.roster()?;
    if r.roster.network_id != authority(id)?.network_id || r.roster.manager_id != id.device_id {
        bail!("MANAGER_STATE_MISMATCH: authority does not belong to this identity")
    }
    current(id)?.check_successor(&r)?;
    Ok(manager)
}
pub fn invitation_link(roster: &SignedRoster, token: &str) -> Result<String> {
    let r = &roster.roster;
    let addresses = r
        .relay_addresses
        .iter()
        .map(|s| {
            s.strip_prefix("https://")
                .context("INVALID_RELAY: HTTPS required")
        })
        .collect::<Result<Vec<_>>>()?
        .join(",");
    Ok(format!(
        "xrun://{addresses}/{}/{}/{}/{}#{token}",
        r.network_id,
        r.manager_id,
        crypto::ca_spki_pin(&roster.ca_pem)?,
        crypto::ca_spki_pin(&r.relay_ca_pem)?
    ))
}
pub async fn invite(id: &Identity, allow: bool) -> Result<serde_json::Value> {
    let manager = manager(id)?;
    let roster = manager.roster()?;
    observe(id, &roster)?;
    synchronize(&roster).await?;
    let token = manager.invite(allow)?;
    Ok(serde_json::json!({"link":invitation_link(&roster,&token)?,"allow":allow,"expires_in":600}))
}
pub async fn revoke(id: &Identity, selector: &str) -> Result<serde_json::Value> {
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
    Ok(
        serde_json::json!({"device_id":target,"revoked":true,"roster_version":roster.roster.version,"undelivered":undelivered,"sync_error":sync_error}),
    )
}

struct Invitation {
    addresses: Vec<String>,
    network: String,
    manager: String,
    root_pin: String,
    relay_pin: String,
    token: String,
}
fn parse_link(link: &str) -> Result<Invitation> {
    let rest = link
        .strip_prefix("xrun://")
        .context("INVALID_LINK: expected xrun://")?;
    let (rest, token) = rest
        .split_once('#')
        .context("INVALID_LINK: missing token")?;
    let mut parts: Vec<_> = rest.rsplitn(5, '/').collect();
    parts.reverse();
    if parts.len() != 5 {
        bail!("INVALID_LINK: expected an end-to-end network invitation")
    }
    let pin_valid = |s: &str, n| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
    };
    if !pin_valid(parts[3], 52)
        || !pin_valid(parts[4], 52)
        || !pin_valid(token, 26)
        || parts[1] != format!("net_{}", parts[3])
    {
        bail!("INVALID_LINK: malformed network fingerprint or token")
    }
    crate::membership::device_name(parts[2])?;
    let addresses = parts[0]
        .split(',')
        .map(|a| {
            let (host, route) = a
                .split_once('/')
                .context("INVALID_LINK: missing relay route")?;
            crate::client::validate_address(host)?;
            if !crate::relay::valid_route(route) {
                bail!("INVALID_LINK: invalid relay route")
            }
            Ok(format!("https://{a}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if addresses.is_empty() || addresses.len() > 8 {
        bail!("INVALID_LINK: invalid relay address count")
    }
    Ok(Invitation {
        addresses,
        network: parts[1].into(),
        manager: parts[2].into(),
        root_pin: parts[3].into(),
        relay_pin: parts[4].into(),
        token: token.into(),
    })
}
async fn pairing_at(
    address: &str,
    relay_ca: &str,
    network: &str,
    root_pin: &str,
    manager: &str,
) -> Result<(Ws, String)> {
    let path = format!("/networks/{network}/connect/{manager}");
    let mut outer =
        net::websocket_at(address, &path, crypto::anonymous_tls_config(relay_ca)?).await?;
    if !matches!(receive(&mut outer).await?, RelayMessage::Connected) {
        bail!("INVALID_MESSAGE: expected pairing tunnel")
    }
    secure::pairing_client(outer, root_pin, manager).await
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
        bail!("IDENTITY_MISMATCH: pairing returned another authority")
    }
    let fp = crypto::csr_key(&crypto::renew_device_request(key)?)?;
    if pair.member.key_fp != fp || pair.member != *pair.roster.member(&pair.member.device_id)? {
        bail!("IDENTITY_MISMATCH: pairing returned another member key")
    }
    crypto::verify_member_certificate(&pair.cert_pem, &pair.roster.ca_pem, &pair.member.device_id)?;
    pair.roster.peer(
        &crypto::cert_der(&pair.cert_pem)?,
        Some(&pair.member.device_id),
    )?;
    pair.receipt.verify(&pair.roster, &pair.member.device_id)?;
    Ok(())
}
pub async fn join(link: &str, name: String) -> Result<Identity> {
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
            bail!("NETWORK_MISMATCH: already joined to another network")
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
            bail!("NETWORK_MISMATCH: pending pairing belongs to another network")
        };
        p.key_pem
    } else {
        crypto::new_device_request()?.0
    };
    let mut error = None;
    for address in &net::ordered_addresses(&invite.addresses, &invite.root_pin) {
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            let relay_ca = crypto::discover_ca(address, &invite.relay_pin).await?;
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
                    token: invite.token.clone(),
                    name: name.clone(),
                    csr_base64: STANDARD.encode(crypto::renew_device_request(&key)?),
                },
            )
            .await?;
            let value: serde_json::Value = net::receive(&mut ws).await?;
            if let Ok(Data::Error { code, message }) = serde_json::from_value(value.clone()) {
                bail!("{code}: {message}")
            }
            let pair: Pairing = serde_json::from_value(value)?;
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
                bail!("IDENTITY_MISMATCH: pairing changed existing identity")
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
                "PAIRING_TIMEOUT: retry using the same invitation and pending key"
            ))
        });
        match result {
            Ok(id) => return Ok(id),
            Err(e) if net::explicit(&e) => return Err(e),
            Err(e) => error = Some(e),
        }
    }
    Err(error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: no relay is reachable")))
}
fn archive_legacy(dir: &std::path::Path) -> Result<()> {
    let old = std::fs::read(dir.join("identity.toml"))?;
    config::atomic_private_write(&dir.join("identity.previous.toml"), &old)?;
    // Preserve jobs, logs and execution settings; old-network grants must not
    // silently turn into permission for current and future new-network members.
    config::update_daemon_config(dir, |cfg| {
        cfg.allow_from.clear();
        cfg.deny_from.clear();
        cfg.allow_all = false;
        Ok(())
    })?;
    if dir.join("daemon.initialized").exists() {
        std::fs::remove_file(dir.join("daemon.initialized"))?;
    }
    Ok(())
}
pub async fn renew(id: &mut Identity) -> Result<()> {
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
                            token: String::new(),
                            name: id.name.clone(),
                            csr_base64: STANDARD.encode(crypto::renew_device_request(&id.key_pem)?),
                        },
                    )
                    .await?;
                    let value: serde_json::Value = net::receive(&mut ws).await?;
                    if let Ok(Data::Error { code, message }) = serde_json::from_value(value.clone())
                    {
                        bail!("{code}: {message}")
                    }
                    let pair: Pairing = serde_json::from_value(value)?;
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
                        "RENEW_TIMEOUT: manager did not complete renewal"
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
                error.unwrap_or_else(|| anyhow::anyhow!("MANAGER_OFFLINE: manager is unavailable"))
            })?
        };
        if pair.member.device_id != id.device_id {
            bail!("IDENTITY_MISMATCH: renewal changed the device ID")
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
        Err(e) => Err(e.context("CERTIFICATE_EXPIRED: renewal requires the manager")),
    }
}
struct RelayEndpoint {
    addresses: Vec<String>,
    pin: String,
}
fn endpoint(link: &str) -> Result<RelayEndpoint> {
    let value = link.strip_prefix("xrun-relay://").context(
        "INVALID_RELAY: use the deployment link printed by xrun relay install or xrun relay invite",
    )?;
    let (value, route) = value
        .split_once('#')
        .context("INVALID_RELAY: missing relay route")?;
    let (addresses, pin) = value
        .split_once('/')
        .context("INVALID_RELAY: missing transport fingerprint")?;
    let valid = |s: &str, n| {
        s.len() == n
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
    };
    if !valid(pin, 52) || !crate::relay::valid_route(route) {
        bail!("INVALID_RELAY: malformed fingerprint or relay route")
    }
    let addresses = addresses
        .split(',')
        .map(|a| {
            crate::client::validate_address(a)?;
            Ok(format!("https://{a}/{route}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if addresses.is_empty() || addresses.len() > 8 {
        bail!("INVALID_RELAY: invalid address count")
    }
    Ok(RelayEndpoint {
        addresses,
        pin: pin.into(),
    })
}
pub async fn create(relay_link: &str, name: Option<String>) -> Result<Identity> {
    let endpoint = endpoint(relay_link)?;
    let mut root = None;
    let mut error = None;
    for address in &endpoint.addresses {
        match tokio::time::timeout(Duration::from_secs(5), async {
            let ca = crypto::discover_ca(address, &endpoint.pin).await?;
            let mut ws = net::websocket_at(
                address,
                "/networks/probe/status",
                crypto::anonymous_tls_config(&ca)?,
            )
            .await?;
            if !matches!(receive(&mut ws).await?, RelayMessage::Status { .. }) {
                bail!("INVALID_MESSAGE: expected relay routes")
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
            Err(_) => error = Some(anyhow::anyhow!("CONNECT_TIMEOUT: relay did not respond")),
        }
    }
    let relay_ca = root.ok_or_else(|| {
        error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: relay is unavailable"))
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
            bail!("INVALID_REQUEST: device names are immutable within a network")
        }
        let manager = manager(old)?;
        let before = manager.roster()?;
        let next = if before.roster.relay_addresses == endpoint.addresses
            && crypto::ca_spki_pin(&before.roster.relay_ca_pem)? == endpoint.pin
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
            bail!("MANAGER_STATE_EXISTS: no matching local identity; refusing authority recovery")
        }
        let key = std::fs::read_to_string(manager_dir.join("device.key.pending"))
            .context("MANAGER_STATE_MISSING: initial device key is missing")?;
        let cert = std::fs::read_to_string(manager_dir.join("device.pem.pending"))?;
        let member = roster.member(&roster.roster.manager_id)?.clone();
        if member.key_fp != crypto::csr_key(&crypto::renew_device_request(&key)?)?
            || member.key_fp != crypto::peer_identity(&crypto::cert_der(&cert)?)?.1
        {
            bail!("MANAGER_STATE_MISMATCH: initial identity differs from the authority")
        }
        if member.name != name
            || roster.roster.relay_addresses != endpoint.addresses
            || crypto::ca_spki_pin(&roster.roster.relay_ca_pem)? != endpoint.pin
        {
            bail!("MANAGER_STATE_MISMATCH: retry initial creation with the same name and relay")
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
pub async fn serve_pair(id: &Identity, ws: &mut Ws) -> Result<()> {
    let manager = manager(id)?;
    let request: PairRequest =
        tokio::time::timeout(Duration::from_secs(10), net::receive(ws)).await??;
    if request.version != VERSION {
        bail!("VERSION_MISMATCH: pairing peer release differs")
    }
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
    if let Err(error) = synchronize(&pair.roster).await {
        tracing::warn!(%error,"membership committed but peer synchronization failed");
    }
    net::send(ws, &pair).await?;
    Ok(())
}
pub async fn http<T: DeserializeOwned>(
    id: &Identity,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<T> {
    let value = match (method, path) {
        (reqwest::Method::GET, "/devices") => serde_json::to_value(devices(id).await?.0)?,
        (reqwest::Method::POST, "/invites") => {
            invite(
                id,
                body.as_ref()
                    .and_then(|v| v["allow"].as_bool())
                    .unwrap_or(false),
            )
            .await?
        }
        (reqwest::Method::POST, "/admin/revoke") => {
            revoke(
                id,
                body.as_ref()
                    .and_then(|v| v["device"].as_str())
                    .context("INVALID_REQUEST: missing device")?,
            )
            .await?
        }
        (reqwest::Method::GET, path) if path.starts_with("/devices/") => {
            let selector = &path[9..];
            let roster = current(id)?;
            let member = roster.member(selector)?;
            if member.revoked {
                bail!("DEVICE_REVOKED: target has been revoked")
            }
            let (devices, _) = devices(id).await?;
            serde_json::to_value(
                devices
                    .into_iter()
                    .find(|d| d.device_id == member.device_id)
                    .context("UNKNOWN_DEVICE: device is not registered")?,
            )?
        }
        _ => bail!("INVALID_REQUEST: unsupported network operation"),
    };
    Ok(serde_json::from_value(value)?)
}
