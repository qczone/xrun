//! Network operations backed by manager authority and encrypted peer sessions.
mod bootstrap;
mod links;
mod pairing;
mod peers;
mod transport;

pub(crate) use bootstrap::create;
#[cfg(test)]
use links::invitation_link;
pub use pairing::{invite, manager};
pub(crate) use pairing::{join, renew, revoke, serve_pair};
pub use peers::PeerState;
pub(crate) use peers::{
    connected_devices, device, devices, local_device, refresh_one, synchronize,
};
pub use transport::authenticate;
pub(crate) use transport::{attach, control, receive, session, session_with_expiry};

use crate::error::ErrorCode;
use crate::{
    config::{self, Identity, NetworkIdentity},
    crypto,
    membership::{RosterCache, SignedRoster},
};
use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;

pub(crate) fn authority(id: &Identity) -> Result<&NetworkIdentity> {
    let network = id.network.as_ref().context(
        ErrorCode::MigrationRequired
            .error("stop old services, then create or join an end-to-end network"),
    )?;
    if network.network_id != format!("net_{}", crypto::ca_spki_pin(&id.ca_pem)?) {
        bail!(ErrorCode::NetworkMismatch.error("identity root differs from its network ID"))
    }
    Ok(network)
}
pub(crate) fn cache() -> Result<RosterCache> {
    RosterCache::open(&config::device_dir()?.join("roster.db"))
}
pub(crate) fn current(id: &Identity) -> Result<SignedRoster> {
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
pub(crate) async fn http<T: DeserializeOwned>(
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
mod tests {
    use super::links::{endpoint, parse_link};
    use super::*;
    use crate::membership::Manager;

    #[test]
    fn public_relay_preserves_network_identity_through_pairing_and_revocation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let address = format!("https://relay.example/{}", crate::crypto::random_token());
        let (manager, _, _, _) = Manager::create(
            &dir.path().join("manager"),
            "manager1",
            vec![address.clone()],
            String::new(),
        )?;
        let roster = manager.roster()?;
        let link = invitation_link(&roster, &crate::crypto::random_token())?;
        let invitation = parse_link(&link)?;
        assert_eq!(invitation.addresses, vec![address.clone()]);
        assert_eq!(invitation.relay_pin, "webpki");
        assert_eq!(invitation.root_pin, crypto::ca_spki_pin(&roster.ca_pem)?);
        let endpoint = endpoint(&address)?;
        assert_eq!(endpoint.addresses, vec![address]);
        assert_eq!(endpoint.pin, "webpki");
        // The manager's signature binds the transport trust policy too.
        let mut tampered = roster.clone();
        tampered.roster.relay_ca_pem = roster.ca_pem.clone();
        assert!(tampered.verify(&roster.roster.network_id).is_err());
        let cache = RosterCache::open(&dir.path().join("roster.db"))?;
        cache.observe(&roster.roster.network_id, &roster)?;
        let (_, csr) = crypto::new_device_request()?;
        let paired = manager.pair(&manager.invite(false)?, "member1", &csr)?;
        cache.observe(&roster.roster.network_id, &paired.roster)?;
        drop(manager);
        let manager = Manager::open(&dir.path().join("manager"))?;
        let revoked = manager.revoke("member1")?;
        cache.observe(&roster.roster.network_id, &revoked)?;
        assert!(
            cache
                .load(&roster.roster.network_id)?
                .member("member1")?
                .revoked
        );
        assert!(manager.pair("", "member1", &csr).is_err());
        Ok(())
    }

    #[test]
    fn public_relay_requires_an_unambiguous_secret_route() {
        for value in [
            "https://relay.example",
            "https://relay.example/wrong",
            "http://relay.example/abcdefghijklmnopqrstuvwxyz",
            "https://user@relay.example/abcdefghijklmnopqrstuvwxyz",
            "https://relay.example/abcdefghijklmnopqrstuvwxyz/",
            "https://relay.example/abcdefghijklmnopqrstuvwxyz?x=1",
            "https://relay.example/abcdefghijklmnopqrstuvwxyz#x",
        ] {
            assert!(endpoint(value).is_err(), "accepted {value}");
        }
    }
}
