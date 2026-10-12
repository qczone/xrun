//! Root-signed freshness bounds stale revocation, including cached and direct sessions.
use super::{
    Manager, SignedRoster,
    records::{sign, verify},
};
use crate::{error::ErrorCode, protocol::now_ms};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

pub(crate) const DEFAULT_LEASE_MS: i64 = 15 * 60 * 1000;
const MAX_LEASE_MS: i64 = 24 * 60 * 60 * 1000;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Freshness {
    pub network_id: String,
    pub authority_generation: u64,
    pub version: u64,
    pub hash: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedLease {
    pub lease: Freshness,
    pub signature: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorityState {
    pub roster: SignedRoster,
    pub lease: SignedLease,
}
impl SignedLease {
    pub fn verify(&self, roster: &SignedRoster) -> Result<()> {
        let lease = &self.lease;
        if lease.network_id != roster.roster.network_id
            || lease.authority_generation != roster.roster.authority_generation
            || lease.version != roster.roster.version
            || lease.hash != roster.hash()?
            || lease.issued_at_ms <= 0
            || lease.expires_at_ms <= lease.issued_at_ms
            || lease.expires_at_ms - lease.issued_at_ms > MAX_LEASE_MS
        {
            bail!(ErrorCode::InvalidRoster.error("freshness lease differs from the signed roster"));
        }
        verify(&roster.ca_pem, "roster-lease", lease, &self.signature)
    }
    pub fn require_fresh(&self, roster: &SignedRoster, now: i64) -> Result<()> {
        self.verify(roster)?;
        if self.lease.issued_at_ms > now + 30_000 || self.lease.expires_at_ms <= now {
            bail!(ErrorCode::RosterExpired.error("contact the manager to renew membership freshness before starting another operation"));
        }
        Ok(())
    }
}
impl AuthorityState {
    pub fn verify(&self, network: &str) -> Result<()> {
        self.roster.verify(network)?;
        self.lease.verify(&self.roster)
    }
    pub fn require_fresh(&self, now: i64) -> Result<()> {
        self.lease.require_fresh(&self.roster, now)
    }
}
impl Manager {
    pub(crate) fn lease_for(&self, roster: &SignedRoster, lifetime: i64) -> Result<SignedLease> {
        if !(60_000..=MAX_LEASE_MS).contains(&lifetime) {
            bail!(ErrorCode::InvalidConfig.error("membership lease must last 1..1440 minutes"));
        }
        let now = now_ms();
        let lease = Freshness {
            network_id: roster.roster.network_id.clone(),
            authority_generation: roster.roster.authority_generation,
            version: roster.roster.version,
            hash: roster.hash()?,
            issued_at_ms: now,
            expires_at_ms: now + lifetime,
        };
        Ok(SignedLease {
            signature: sign(&self.key_pem, "roster-lease", &lease)?,
            lease,
        })
    }
    pub fn authority_state(&self, lifetime: i64) -> Result<AuthorityState> {
        let roster = self.roster()?;
        Ok(AuthorityState {
            lease: self.lease_for(&roster, lifetime)?,
            roster,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn leases_bind_version_generation_hash_and_expiry_and_persist_after_restart() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let manager = Manager::create(
            &dir.path().join("manager"),
            "manager1",
            vec!["https://relay.example.com".into()],
            String::new(),
        )?
        .0;
        let state = manager.authority_state(DEFAULT_LEASE_MS)?;
        state.require_fresh(state.lease.lease.issued_at_ms)?;
        let error = state
            .require_fresh(state.lease.lease.expires_at_ms)
            .unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::RosterExpired));
        let mut corrupted = state.clone();
        corrupted.lease.lease.authority_generation += 1;
        assert!(corrupted.verify(&state.roster.roster.network_id).is_err());
        let path = dir.path().join("roster.db");
        let cache = super::super::RosterCache::open(&path)?;
        cache.observe_authority(&state.roster.roster.network_id, &state)?;
        drop(cache);
        let cache = super::super::RosterCache::open(&path)?;
        assert_eq!(
            cache
                .require_fresh(
                    &state.roster.roster.network_id,
                    state.lease.lease.issued_at_ms
                )?
                .lease
                .lease
                .hash,
            state.lease.lease.hash
        );
        assert!(
            cache
                .require_fresh(
                    &state.roster.roster.network_id,
                    state.lease.lease.expires_at_ms
                )
                .is_err()
        );
        Ok(())
    }
}
