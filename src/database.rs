//! Fresh schema initialization and explicit rejection of unsupported databases.
use crate::error::ErrorCode;
use anyhow::{Result, bail};
use rusqlite::Connection;

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
    let tables: i64 = db.query_row(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if version != 0 || tables != 0 {
        bail!(ErrorCode::DbSchemaMismatch.error(format!(
            "{kind} schema {version} is unsupported; expected {supported}. Recreate the database explicitly while stopped."
        )));
    }
    let transaction = db.transaction()?;
    transaction.execute_batch(schema)?;
    transaction.pragma_update(None, "user_version", supported)?;
    transaction.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
