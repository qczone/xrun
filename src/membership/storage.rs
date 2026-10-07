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
    crate::database::initialize(&mut db, "membership", 1, "CREATE TABLE state(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL);
        CREATE TABLE invitations(hash TEXT PRIMARY KEY,expires INTEGER NOT NULL,allow INTEGER NOT NULL);
        CREATE TABLE receipts(device TEXT PRIMARY KEY,data TEXT NOT NULL);")?;
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
