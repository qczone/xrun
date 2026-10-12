//! Durable public-route throttling uses the accepted socket IP, never a client header.
use crate::{
    database,
    error::ErrorCode,
    protocol::{now_ms, sha256},
};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{path::Path, sync::Mutex};

pub(super) struct PairingLimits(Mutex<Connection>);
impl PairingLimits {
    pub(super) fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        database::configure(&db)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS pairing_peers (peer TEXT PRIMARY KEY,expires INTEGER NOT NULL,hits INTEGER NOT NULL) STRICT;")?;
        Ok(Self(Mutex::new(db)))
    }
    pub(super) fn check(&self, ip: std::net::IpAddr) -> Result<()> {
        self.check_at(&sha256(ip.to_string().as_bytes()), now_ms())
    }
    fn check_at(&self, peer: &str, now: i64) -> Result<()> {
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM pairing_peers WHERE expires<=?1", [now])?;
        let row: Option<(i64, i64)> = tx
            .query_row(
                "SELECT expires,hits FROM pairing_peers WHERE peer=?1",
                [peer],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let (expires, hits) = row.unwrap_or((now + 60_000, 0));
        if hits >= 32
            || row.is_none()
                && tx.query_row("SELECT COUNT(*) FROM pairing_peers", [], |row| {
                    row.get::<_, i64>(0)
                })? >= 4096
        {
            bail!(ErrorCode::PairingBusy.error("public pairing rate exceeded; retry in a minute"));
        }
        tx.execute("INSERT INTO pairing_peers VALUES(?1,?2,?3) ON CONFLICT(peer) DO UPDATE SET hits=excluded.hits",
            params![peer, expires, hits + 1])?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_peer_budget_survives_restart_and_cannot_grow_without_bound() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("pairing.sqlite");
        let rate = PairingLimits::open(&path)?;
        for _ in 0..32 {
            rate.check_at("peer", 1)?;
        }
        assert!(rate.check_at("peer", 2).is_err());
        drop(rate);
        let rate = PairingLimits::open(&path)?;
        assert!(rate.check_at("peer", 3).is_err());
        rate.check_at("other", 3)?;
        rate.check_at("peer", 60_001)?;
        rate.0.lock().unwrap().execute_batch(
            "WITH RECURSIVE peers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM peers WHERE n<4095)
            INSERT INTO pairing_peers SELECT 'fixture-'||n,120001,1 FROM peers;",
        )?;
        assert!(rate.check_at("overflow", 60_002).is_err());
        rate.check_at("recovered", 120_001)?;
        Ok(())
    }
}
