//! Network state and compatibility facade for the public network API.
mod bootstrap;
mod links;
mod pairing;
mod peers;
mod transport;

pub use bootstrap::create;
pub use links::invitation_link;
pub use pairing::{invite, join, manager, renew, revoke, serve_pair};
pub use peers::{
    PeerState, connected_devices, device, devices, local_device, refresh, refresh_one, synchronize,
};
pub(crate) use transport::session_with_expiry;
pub use transport::{attach, authenticate, control, open, receive, session};

use crate::error::ErrorCode;
use crate::{
    config::{self, Identity, NetworkIdentity},
    crypto,
    membership::{RosterCache, SignedRoster},
};
use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

pub fn authority(id: &Identity) -> Result<&NetworkIdentity> {
    let network = id.network.as_ref().context(
        ErrorCode::MigrationRequired
            .error("stop old services, then create or join an end-to-end network"),
    )?;
    if network.network_id != format!("net_{}", crypto::ca_spki_pin(&id.ca_pem)?) {
        bail!(ErrorCode::NetworkMismatch.error("identity root differs from its network ID"))
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
        bail!(
            ErrorCode::MemberStateMissing
                .error("refusing to discard the highest known roster version")
        )
    }
    let value = RosterCache::open(&path)?.load(&network.network_id)?;
    if value.roster.manager_id != network.manager_id {
        bail!(ErrorCode::IdentityMismatch.error("network manager differs from the identity"))
    }
    let self_member = value.member(&id.device_id)?;
    if self_member.key_fp != crypto::peer_identity(&crypto::cert_der(&id.cert_pem)?)?.1 {
        bail!(ErrorCode::IdentityMismatch.error("local key differs from its membership"))
    }
    if self_member.revoked {
        bail!(ErrorCode::DeviceRevoked.error("local identity has been revoked"))
    }
    Ok(value)
}
pub fn observe(id: &Identity, value: &SignedRoster) -> Result<()> {
    let network = authority(id)?;
    if value.roster.manager_id != network.manager_id {
        bail!(ErrorCode::IdentityMismatch.error("signed roster names another manager"))
    }
    match cache()?.observe(&network.network_id, value) {
        Ok(()) => Ok(()),
        Err(e) if crate::error::is(&e, ErrorCode::RosterRollback) => Ok(()),
        Err(e) => Err(e),
    }
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
pub async fn http<T: DeserializeOwned>(
    id: &Identity,
    method: reqwest::Method,
    path: &str,
    body: Option<serde_json::Value>,
) -> Result<T> {
    let value = match (method, path) {
        (reqwest::Method::GET, "/devices") => serde_json::to_value(connected_devices(id).await?)?,
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
                    .context(ErrorCode::InvalidRequest.error("missing device"))?,
            )
            .await?
        }
        (reqwest::Method::GET, path) if path.starts_with("/devices/") => {
            serde_json::to_value(device(id, &path[9..]).await?)?
        }
        _ => bail!(ErrorCode::InvalidRequest.error("unsupported network operation")),
    };
    Ok(serde_json::from_value(value)?)
}
#[cfg(test)]
mod tests;
