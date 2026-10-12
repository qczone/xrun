//! Shared private membership database format and atomic state IO.
use super::SignedRoster;
use crate::{config, error::ErrorCode};
use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use std::path::Path;
pub(super) fn database(path: &Path, create: bool) -> Result<Connection> {
    if !create && !path.exists() {
        bail!(ErrorCode::ManagerStateMissing.error("refusing to recreate network authority"))
    }
    let parent = path.parent().context("missing database parent")?;
    std::fs::create_dir_all(parent)?;
    config::restrict_dir(parent)?;
    let mut db = Connection::open(path)?;
    crate::database::configure(&db)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: i64 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == 0 {
        let tables: i64 = tx.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )?;
        if tables != 0 {
            bail!(ErrorCode::DbSchemaMismatch.error("unknown membership database layout"));
        }
        tx.execute_batch("CREATE TABLE state(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);
            CREATE TABLE invitations(hash TEXT PRIMARY KEY,expires INTEGER NOT NULL,allow INTEGER NOT NULL);
            CREATE TABLE receipts(device TEXT PRIMARY KEY,data TEXT NOT NULL);")?;
    }
    if version == 0 || version == 1 {
        tx.execute_batch(
            "ALTER TABLE invitations ADD COLUMN kind TEXT NOT NULL DEFAULT 'link';
            ALTER TABLE invitations ADD COLUMN invitation_id TEXT;
            ALTER TABLE invitations ADD COLUMN locator TEXT;
            ALTER TABLE invitations ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
            ALTER TABLE invitations ADD COLUMN status TEXT NOT NULL DEFAULT 'active';
            ALTER TABLE invitations ADD COLUMN key_fp TEXT;
            CREATE UNIQUE INDEX invitation_identity ON invitations(invitation_id);",
        )?;
        tx.pragma_update(None, "user_version", 2)?;
    } else if version != 2 && version != 3 {
        bail!(ErrorCode::DbSchemaMismatch.error("unsupported membership database schema"));
    }
    if version <= 2 {
        tx.execute_batch(
            "ALTER TABLE invitations ADD COLUMN next_attempt_ms INTEGER NOT NULL DEFAULT 0;",
        )?;
        tx.pragma_update(None, "user_version", 3)?;
    }
    tx.commit()?;
    Ok(db)
}
pub(super) fn state(db: &Connection) -> Result<SignedRoster> {
    let data: String = db.query_row("SELECT data FROM state WHERE id=1", [], |r| r.get(0))?;
    Ok(serde_json::from_str(&data)?)
}
pub(super) fn save_state(db: &Connection, value: &SignedRoster) -> Result<()> {
    db.execute(
        "INSERT INTO state VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
        [serde_json::to_string(value)?],
    )?;
    Ok(())
}
