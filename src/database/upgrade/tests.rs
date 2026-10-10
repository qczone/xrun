use super::*;

const NEW_SCHEMA: &str = "CREATE TABLE fresh(id INTEGER PRIMARY KEY)";

fn old_database(path: &Path) -> Result<Connection> {
    let db = Connection::open(path)?;
    crate::database::configure(&db)?;
    db.execute_batch(
        "CREATE TABLE parent(id INTEGER PRIMARY KEY);
         CREATE TABLE child(id INTEGER REFERENCES parent(id));
         CREATE VIEW old_view AS SELECT id FROM parent;
         INSERT INTO parent VALUES(42);
         INSERT INTO child VALUES(42);
         PRAGMA user_version=1;",
    )?;
    Ok(db)
}

fn backups(path: &Path) -> Result<Vec<PathBuf>> {
    let root = path.parent().unwrap().join("database-backups");
    if !root.exists() {
        return Ok(vec![]);
    }
    std::fs::read_dir(root)?
        .map(|entry| Ok(entry?.path().join(path.file_name().unwrap())))
        .collect()
}

fn old_value(db: &Connection) -> Result<i64> {
    Ok(db.query_row("SELECT id FROM old_view", [], |row| row.get(0))?)
}

#[test]
fn wal_snapshot_retains_old_rows_before_atomic_rebuild() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.db");
    let writer = old_database(&path)?;
    // Keep this connection alive so rows remain in WAL rather than the main file.
    assert!(path.with_extension("db-wal").metadata()?.len() > 0);
    let mut db = Connection::open(&path)?;
    crate::database::configure(&db)?;
    assert!(older_schema(&path, 2)?);
    initialize_with_backup(&mut db, &path, "test", 2, NEW_SCHEMA)?;
    assert!(!older_schema(&path, 2)?);
    assert_eq!(
        db.pragma_query_value(None, "foreign_keys", |r| r.get::<_, i64>(0))?,
        1
    );
    assert!(db.prepare("SELECT * FROM child").is_err());
    assert!(db.prepare("SELECT * FROM old_view").is_err());
    let copies = backups(&path)?;
    assert_eq!(copies.len(), 1);
    let copy = Connection::open(&copies[0])?;
    assert_eq!(old_value(&copy)?, 42);
    assert_eq!(
        copy.query_row("SELECT id FROM child", [], |r| r.get::<_, i64>(0))?,
        42
    );
    assert_eq!(
        copy.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))?,
        1
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(copies[0].metadata()?.permissions().mode() & 0o777, 0o600);
        assert_eq!(
            copies[0].parent().unwrap().metadata()?.permissions().mode() & 0o777,
            0o700
        );
    }
    db.execute("INSERT INTO fresh VALUES(99)", [])?;
    initialize_with_backup(&mut db, &path, "test", 2, NEW_SCHEMA)?;
    assert_eq!(backups(&path)?.len(), 1);
    assert_eq!(
        db.query_row("SELECT id FROM fresh", [], |r| r.get::<_, i64>(0))?,
        99
    );
    drop(writer);
    Ok(())
}

#[test]
fn failed_backup_or_schema_creation_leaves_original_database_intact() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.db");
    let mut db = old_database(&path)?;
    let root = directory.path().join("database-backups");
    std::fs::write(&root, b"block backup directory creation")?;
    assert!(initialize_with_backup(&mut db, &path, "test", 2, NEW_SCHEMA).is_err());
    assert_eq!(old_value(&db)?, 42);
    assert!(older_schema(&path, 2)?);
    std::fs::remove_file(root)?;
    assert!(initialize_with_backup(&mut db, &path, "test", 2, "INVALID SQL").is_err());
    assert_eq!(old_value(&db)?, 42);
    assert!(older_schema(&path, 2)?);
    assert_eq!(
        db.pragma_query_value(None, "foreign_keys", |r| r.get::<_, i64>(0))?,
        1
    );
    assert_eq!(backups(&path)?.len(), 1);
    Ok(())
}

#[test]
fn future_and_unversioned_databases_are_never_replaced() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("daemon.db");
    let mut db = old_database(&path)?;
    for version in [3, 0] {
        db.pragma_update(None, "user_version", version)?;
        assert!(older_schema(&path, 2).is_err());
        let error = initialize_with_backup(&mut db, &path, "test", 2, NEW_SCHEMA).unwrap_err();
        assert!(crate::error::is(&error, ErrorCode::DbSchemaMismatch));
        assert_eq!(old_value(&db)?, 42);
        assert!(backups(&path)?.is_empty());
    }
    Ok(())
}

#[test]
fn concurrent_upgraders_make_one_snapshot_and_keep_new_writes() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("submissions.sqlite");
    drop(old_database(&path)?);
    let barrier = std::sync::Barrier::new(8);
    std::thread::scope(|scope| -> Result<()> {
        let workers = (0..8)
            .map(|id| {
                let path = &path;
                let barrier = &barrier;
                scope.spawn(move || -> Result<()> {
                    let mut db = Connection::open(path)?;
                    crate::database::configure(&db)?;
                    barrier.wait();
                    initialize_with_backup(&mut db, path, "test", 2, NEW_SCHEMA)?;
                    db.execute("INSERT INTO fresh VALUES(?1)", [id])?;
                    Ok(())
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            worker.join().expect("upgrade worker panicked")?;
        }
        Ok(())
    })?;
    let db = Connection::open(&path)?;
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM fresh", [], |r| r.get::<_, i64>(0))?,
        8
    );
    assert_eq!(backups(&path)?.len(), 1);
    Ok(())
}
