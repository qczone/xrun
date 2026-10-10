//! Schema initialization; disposable history upgrades have a separate backup policy.
mod upgrade;
use crate::error::ErrorCode;
use anyhow::{Result, bail};
use rusqlite::{Connection, TransactionBehavior};
use std::time::{Duration, Instant};
pub(crate) use upgrade::{initialize_with_backup, older_schema};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const WAL_RETRY_DELAY: Duration = Duration::from_millis(10);

pub(crate) fn configure(db: &Connection) -> Result<()> {
    // Concurrent first openers can both hold a read lock while requesting WAL.
    // SQLite can return BUSY immediately for that upgrade, bypassing its busy
    // handler. Retry only this idempotent pragma, within the same total budget.
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        db.busy_timeout(deadline.saturating_duration_since(Instant::now()))?;
        match db.pragma_update(None, "journal_mode", "WAL") {
            Ok(()) => break,
            Err(rusqlite::Error::SqliteFailure(error, _))
                if error.code == rusqlite::ErrorCode::DatabaseBusy && Instant::now() < deadline =>
            {
                std::thread::sleep(
                    WAL_RETRY_DELAY.min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    db.busy_timeout(BUSY_TIMEOUT)?;
    db.pragma_update(None, "synchronous", "FULL")?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

pub(crate) fn initialize(
    db: &mut Connection,
    kind: &str,
    supported: i64,
    schema: &str,
) -> Result<()> {
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == supported {
        return Ok(());
    }
    // Only initialization needs a writer. A competing opener may have completed
    // the schema while we waited, so every decision is repeated under this lock.
    let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version == supported {
        transaction.commit()?;
        return Ok(());
    }
    let tables: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if version != 0 || tables != 0 {
        bail!(ErrorCode::DbSchemaMismatch.error(format!(
            "{kind} schema {version} is unsupported; expected {supported}. \
             Use a matching xrun version or see `xrun doc upgrade` for explicit recovery while stopped."
        )));
    }
    transaction.execute_batch(schema)?;
    transaction.pragma_update(None, "user_version", supported)?;
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_first_openers_share_one_schema_without_losing_rows() -> Result<()> {
        let directory = tempfile::tempdir()?;
        for attempt in 0..8 {
            let path = directory.path().join(format!("fresh-{attempt}.sqlite"));
            let barrier = std::sync::Barrier::new(8);
            std::thread::scope(|scope| -> Result<()> {
                let workers: Vec<_> = (0..8)
                    .map(|id| {
                        let path = &path;
                        let barrier = &barrier;
                        scope.spawn(move || -> Result<()> {
                            let mut db = Connection::open(path)?;
                            db.busy_timeout(std::time::Duration::from_secs(5))?;
                            barrier.wait();
                            initialize(
                                &mut db,
                                "test",
                                1,
                                "CREATE TABLE example(id INTEGER PRIMARY KEY)",
                            )?;
                            db.execute("INSERT INTO example VALUES(?1)", [id])?;
                            Ok(())
                        })
                    })
                    .collect();
                for worker in workers {
                    worker.join().expect("database opener panicked")?;
                }
                Ok(())
            })?;
            let db = Connection::open(path)?;
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM example", [], |row| row
                    .get::<_, i64>(0))?,
                8
            );
        }
        Ok(())
    }

    #[test]
    fn schema_initialization_is_atomic_and_rejects_old_and_future_layouts() -> Result<()> {
        let mut db = Connection::open_in_memory()?;
        assert!(
            initialize(
                &mut db,
                "test",
                1,
                "CREATE TABLE example(id INTEGER); INVALID SQL"
            )
            .is_err()
        );
        let tables: i64 =
            db.query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get(0))?;
        assert_eq!(tables, 0);
        initialize(&mut db, "test", 1, "CREATE TABLE example(id INTEGER)")?;
        db.execute("INSERT INTO example VALUES(42)", [])?;
        db.pragma_update(None, "user_version", 2)?;
        assert!(crate::error::is(
            &initialize(&mut db, "test", 1, "").unwrap_err(),
            ErrorCode::DbSchemaMismatch
        ));
        db.pragma_update(None, "user_version", 0)?;
        assert!(initialize(&mut db, "test", 1, "").is_err());
        assert_eq!(
            db.query_row("SELECT id FROM example", [], |row| row.get::<_, i64>(0))?,
            42
        );
        Ok(())
    }
}
