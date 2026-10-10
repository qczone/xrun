//! Replace older disposable stores only after a durable SQLite snapshot exists.
use crate::{config, error::ErrorCode};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use std::path::{Path, PathBuf};

pub(crate) fn older_schema(path: &Path, supported: i64) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let db = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(super::BUSY_TIMEOUT)?;
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let unversioned = version == 0
        && db.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get::<_, i64>(0),
        )? != 0;
    if version < 0 || version > supported || unversioned {
        bail!(ErrorCode::DbSchemaMismatch.error(format!(
            "{} schema {version} is unsupported; expected {supported}",
            path.display()
        )));
    }
    Ok(version > 0 && version < supported)
}

/// Callers exclude old daemon writers with its instance lock. Concurrent CLI
/// openers are serialized by the SQLite transaction and recheck the version.
pub(crate) fn initialize_with_backup(
    db: &mut Connection,
    path: &Path,
    kind: &str,
    supported: i64,
    schema: &str,
) -> Result<()> {
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == supported || version == 0 {
        return super::initialize(db, kind, supported, schema);
    }
    // DDL removes the entire old layout, without interpreting historical rows.
    // Foreign keys are restored even when snapshotting or initialization fails.
    db.pragma_update(None, "foreign_keys", "OFF")?;
    let result = (|| -> Result<()> {
        let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let version: i64 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == supported {
            transaction.commit()?;
            return Ok(());
        }
        if version <= 0 || version >= supported {
            bail!(ErrorCode::DbSchemaMismatch.error(format!(
                "{kind} schema {version} is unsupported; expected {supported}"
            )));
        }
        // A separate read connection includes committed WAL pages while this
        // transaction excludes writers. Backing up the write connection locks it.
        let backup = snapshot(path, version)?;
        let objects = transaction
            .prepare("SELECT type,name FROM sqlite_schema WHERE type IN ('view','table') AND name NOT LIKE 'sqlite_%' ORDER BY type DESC")?
            .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (kind, name) in objects {
            let name = name.replace('"', "\"\"");
            transaction.execute_batch(&format!("DROP {kind} \"{name}\""))?;
        }
        transaction.execute_batch(schema)?;
        transaction.pragma_update(None, "user_version", supported)?;
        transaction.commit()?;
        tracing::info!(database = %path.display(), backup = %backup.display(), "older history database backed up and rebuilt");
        Ok(())
    })();
    db.pragma_update(None, "foreign_keys", "ON")?;
    result
}

fn snapshot(path: &Path, version: i64) -> Result<PathBuf> {
    let parent = path.parent().context("missing database parent")?;
    let root = parent.join("database-backups");
    std::fs::create_dir_all(&root)?;
    config::restrict_dir(&root)?;
    let directory = root.join(format!(
        "{}-v{version}-{}-{}",
        path.file_name()
            .context("missing database name")?
            .to_string_lossy(),
        crate::protocol::now_ms(),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir(&directory)?;
    config::restrict_dir(&directory)?;
    let backup = directory.join(path.file_name().context("missing database name")?);
    config::atomic_private_write(&backup, b"")?;
    let source = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    source.busy_timeout(super::BUSY_TIMEOUT)?;
    source.backup(rusqlite::MAIN_DB, &backup, None)?;
    let copy = Connection::open(&backup)?;
    let integrity: String = copy.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        bail!(ErrorCode::DbCorrupt.error(format!("history backup: {integrity}")));
    }
    drop(copy);
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&backup)?
        .sync_all()?;
    config::sync_parent(&backup)?;
    config::sync_parent(&directory)?;
    config::sync_parent(&root)?;
    Ok(backup)
}

#[cfg(test)]
mod tests;
