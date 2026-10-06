//! Verified monotonic roster cache and committed-change notifications.
use super::{SignedRoster, storage::*};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use std::{path::Path, sync::Mutex};
struct CachedRoster {
    db: Connection,
    data_version: i64,
    verified: Option<(String, SignedRoster)>,
}
pub struct RosterCache {
    state: Mutex<CachedRoster>,
    changes: tokio::sync::watch::Sender<()>,
}
impl RosterCache {
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            state: Mutex::new(CachedRoster {
                db: database(path, true)?,
                data_version: -1,
                verified: None,
            }),
            changes: tokio::sync::watch::channel(()).0,
        })
    }
    pub fn load(&self, network: &str) -> Result<SignedRoster> {
        let mut cached = self.state.lock().unwrap();
        let version = cached
            .db
            .pragma_query_value(None, "data_version", |row| row.get(0))?;
        if cached.data_version != version
            || cached.verified.as_ref().is_none_or(|(id, _)| id != network)
        {
            let next = state(&cached.db)?;
            next.verify(network)?;
            if let Some((id, previous)) = &cached.verified
                && id == network
            {
                previous.check_successor(&next)?;
            }
            cached.verified = Some((network.into(), next));
            cached.data_version = version;
        }
        Ok(cached.verified.as_ref().unwrap().1.clone())
    }
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes.subscribe()
    }
    pub fn observe(&self, network: &str, next: &SignedRoster) -> Result<()> {
        next.verify(network)?;
        let mut cached = self.state.lock().unwrap();
        let observed_version = cached
            .db
            .pragma_query_value(None, "data_version", |row| row.get(0))?;
        let tx = cached
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old: Option<String> = tx
            .query_row("SELECT data FROM state WHERE id=1", [], |r| r.get(0))
            .optional()?;
        if let Some(old) = old {
            let previous = serde_json::from_str::<SignedRoster>(&old)?;
            previous.check_successor(next)?;
            if previous.roster.version == next.roster.version {
                return Ok(());
            }
        }
        save_state(&tx, next)?;
        tx.commit()?;
        // Our own writes do not change PRAGMA data_version. Publish the verified
        // value after commit; writes from other processes invalidate it on load.
        cached.verified = Some((network.into(), next.clone()));
        cached.data_version = observed_version;
        self.changes.send_replace(());
        Ok(())
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use crate::membership::Manager;

    #[test]
    fn verified_cache_detects_external_writes_and_notifies_only_after_commit() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let manager = Manager::create(
            &dir.path().join("manager"),
            "manager1",
            vec!["https://relay.example.com".into()],
            String::new(),
        )?
        .0;
        let initial = manager.roster()?;
        let network = &initial.roster.network_id;
        let cache = RosterCache::open(&dir.path().join("roster.db"))?;
        let external = RosterCache::open(&dir.path().join("roster.db"))?;
        let changes = cache.subscribe();
        cache.observe(network, &initial)?;
        assert!(changes.has_changed()?);
        assert_eq!(cache.load(network)?.roster.version, 1);
        let next = manager.set_relay(vec!["https://other.example.com".into()], String::new())?;
        external.observe(network, &next)?;
        assert_eq!(cache.load(network)?.roster.version, 2);
        let mut forged = next.clone();
        forged.roster.members[0].name = "forged".into();
        assert!(cache.observe(network, &forged).is_err());
        assert_eq!(cache.load(network)?.roster.version, 2);
        Ok(())
    }
}
