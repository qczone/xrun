//! Upgrade scenarios use synthetic old databases, isolated homes and a local helper.
use super::*;
use rusqlite::Connection;

#[cfg(target_os = "macos")]
struct StopFixture;
#[cfg(target_os = "macos")]
impl Drop for StopFixture {
    fn drop(&mut self) {
        let _ = tauri::async_runtime::block_on(xrun::client::services::stop_daemon());
    }
}

fn old_store(path: &Path) -> Result<()> {
    // Only disposable fixture databases are replaced; the seeded identity and
    // member/authorization files remain intact for the preservation assertions.
    for suffix in ["", "-wal", "-shm"] {
        let path = Path::new(&format!("{}{suffix}", path.display())).to_owned();
        if path.exists() {
            std::fs::remove_file(path)?;
        }
    }
    let db = Connection::open(path)?;
    db.execute_batch(
        "CREATE TABLE old_records(id TEXT PRIMARY KEY,payload TEXT);
         INSERT INTO old_records VALUES('old','retained snapshot');
         PRAGMA user_version=1;",
    )?;
    Ok(())
}

fn backups(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    std::fs::read_dir(dir.join("database-backups"))?
        .map(|entry| Ok(entry?.path()))
        .collect()
}

fn verify_snapshot(directory: &Path, file: &str) -> Result<()> {
    let db = Connection::open(directory.join(file))?;
    assert_eq!(
        db.query_row("SELECT payload FROM old_records", [], |row| row
            .get::<_, String>(0))?,
        "retained snapshot"
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[test]
fn activity_open_prepares_old_stores_without_starting_a_stopped_service() -> Result<()> {
    isolated(
        "activity_open_prepares_old_stores_without_starting_a_stopped_service",
        || {
            let root = config::home_dir()?;
            seed(&relay_config(&root)?)?;
            let dir = config::device_dir()?;
            let identity = std::fs::read(dir.join("identity.toml"))?;
            let policy = std::fs::read(dir.join("daemon.toml"))?;
            let roster = std::fs::read(dir.join("roster.db"))?;
            old_store(&dir.join("daemon.db"))?;
            old_store(&dir.join("submissions.sqlite"))?;
            let (_app, window) = build_app();
            let page = invoke(&window, "activity_history", json!({"filter":"all"})).unwrap();
            assert_eq!(page["entries"], json!([]));
            assert!(page["db_id"].as_str().unwrap().starts_with("db_"));
            assert!(!xrun::client::services::daemon_running()?);
            assert_eq!(std::fs::read(dir.join("identity.toml"))?, identity);
            assert_eq!(std::fs::read(dir.join("daemon.toml"))?, policy);
            assert_eq!(std::fs::read(dir.join("roster.db"))?, roster);
            let copies = backups(&dir)?;
            assert_eq!(copies.len(), 2);
            for copy in copies {
                let file = if copy.join("daemon.db").exists() {
                    "daemon.db"
                } else {
                    "submissions.sqlite"
                };
                verify_snapshot(&copy, file)?;
            }
            assert_eq!(
                invoke(&window, "activity_history", json!({"filter":"all"})),
                Ok(page)
            );
            assert_eq!(backups(&dir)?.len(), 2);
            Ok(())
        },
    )
}

#[test]
fn failed_activity_upgrade_preserves_old_rows_and_retries() -> Result<()> {
    isolated(
        "failed_activity_upgrade_preserves_old_rows_and_retries",
        || {
            let root = config::home_dir()?;
            seed(&relay_config(&root)?)?;
            let dir = config::device_dir()?;
            old_store(&dir.join("daemon.db"))?;
            std::fs::write(
                dir.join("database-backups"),
                b"block backup directory creation",
            )?;
            let (_app, window) = build_app();
            assert!(invoke(&window, "activity_history", json!({"filter":"all"})).is_err());
            verify_snapshot(&dir, "daemon.db")?;
            config::atomic_private_write(&dir.join("daemon-upgrade-resume"), b"resume\n")?;
            assert!(xrun::client::services::daemon_upgrade_pending()?);
            assert_eq!(invoke(&window, "stop", json!({})), Ok(Value::Null));
            assert!(!xrun::client::services::daemon_upgrade_pending()?);
            std::fs::remove_file(dir.join("database-backups"))?;
            let page = invoke(&window, "activity_history", json!({"filter":"all"})).unwrap();
            assert_eq!(page["entries"], json!([]));
            assert_eq!(backups(&dir)?.len(), 1);
            assert!(!xrun::client::services::daemon_running()?);
            Ok(())
        },
    )
}

#[cfg(target_os = "macos")]
fn build_helper() -> Result<std::path::PathBuf> {
    let source = config::home_dir()?.join("helper.rs");
    std::fs::write(
        &source,
        include_str!("../../../tests/fixtures/dev-daemon.rs"),
    )?;
    let helper = std::env::current_exe()?.with_file_name("xrun");
    let output = Command::new("rustc")
        .arg(source)
        .arg("-o")
        .arg(&helper)
        .env("XRUN_FIXTURE_VERSION", xrun::protocol::VERSION)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(helper)
}

#[cfg(target_os = "macos")]
#[test]
fn activity_upgrade_switches_the_old_running_daemon_and_survives_a_retry() -> Result<()> {
    isolated(
        "activity_upgrade_switches_the_old_running_daemon_and_survives_a_retry",
        || {
            let root = config::home_dir()?;
            seed(&relay_config(&root)?)?;
            let dir = config::device_dir()?;
            old_store(&dir.join("daemon.db"))?;
            let helper = build_helper()?;
            let _cleanup = StopFixture;
            let mut old = Command::new(&helper)
                .arg("daemon")
                .current_dir(&dir)
                .spawn()?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while xrun::client::services::daemon_state(&dir)?
                .generation
                .is_none()
            {
                anyhow::ensure!(Instant::now() < deadline, "fixture did not become ready");
                std::thread::sleep(Duration::from_millis(20));
            }
            let previous = std::fs::read(dir.join("pid"))?;
            let (_app, window) = build_app();
            // A storage failure must preserve both old data and the restart intent.
            std::fs::write(
                dir.join("database-backups"),
                b"block backup directory creation",
            )?;
            assert!(invoke(&window, "activity_history", json!({"filter":"all"})).is_err());
            assert!(old.wait()?.success());
            assert!(!xrun::client::services::daemon_running()?);
            assert!(xrun::client::services::daemon_upgrade_pending()?);
            verify_snapshot(&dir, "daemon.db")?;
            std::fs::remove_file(dir.join("database-backups"))?;
            // Reopening the App retries using the persisted intent, not process state.
            let (_reopened, window) = build_app();
            let page = invoke(&window, "activity_history", json!({"filter":"all"})).unwrap();
            assert_eq!(page["entries"], json!([]));
            assert!(xrun::client::services::daemon_running()?);
            assert!(!xrun::client::services::daemon_upgrade_pending()?);
            assert_ne!(std::fs::read(dir.join("pid"))?, previous);
            let copies = backups(&dir)?;
            assert_eq!(copies.len(), 1);
            verify_snapshot(&copies[0], "daemon.db")?;
            assert_eq!(invoke(&window, "stop", json!({})), Ok(Value::Null));
            assert!(!xrun::client::services::daemon_running()?);
            Ok(())
        },
    )
}

#[test]
fn storage_upgrade_requires_exclusive_daemon_ownership() -> Result<()> {
    isolated(
        "storage_upgrade_requires_exclusive_daemon_ownership",
        || {
            let root = config::home_dir()?;
            seed(&relay_config(&root)?)?;
            let dir = config::device_dir()?;
            old_store(&dir.join("daemon.db"))?;
            let lock = xrun::testing::daemon::instance_lock()?;
            assert!(xrun::client::services::prepare_storage_upgrade().is_err());
            verify_snapshot(&dir, "daemon.db")?;
            assert!(!dir.join("database-backups").exists());
            drop(lock);
            xrun::client::services::initialize_daemon()?;
            assert_eq!(
                xrun::testing::store::JobStore::open(&dir.join("daemon.db"), false)?
                    .all()?
                    .len(),
                0
            );
            assert_eq!(backups(&dir)?.len(), 1);
            Ok(())
        },
    )
}

#[test]
fn unknown_storage_is_rejected_before_stopping_a_running_daemon() -> Result<()> {
    isolated(
        "unknown_storage_is_rejected_before_stopping_a_running_daemon",
        || {
            let root = config::home_dir()?;
            seed(&relay_config(&root)?)?;
            let dir = config::device_dir()?;
            let job_path = dir.join("daemon.db");
            old_store(&job_path)?;
            let submissions = dir.join("submissions.sqlite");
            old_store(&submissions)?;
            Connection::open(&submissions)?.pragma_update(None, "user_version", 3)?;
            let _lock = xrun::testing::daemon::instance_lock()?;
            let (_app, window) = build_app();
            let error = invoke(&window, "activity_history", json!({"filter":"all"})).unwrap_err();
            assert_eq!(error["code"], "DB_SCHEMA_MISMATCH");
            assert!(xrun::client::services::daemon_running()?);
            assert!(!xrun::client::services::daemon_upgrade_pending()?);
            assert!(!dir.join("database-backups").exists());
            verify_snapshot(&dir, "daemon.db")?;
            assert_eq!(
                Connection::open(&submissions)?.pragma_query_value(
                    None,
                    "user_version",
                    |row| row.get::<_, i64>(0)
                )?,
                3
            );
            Ok(())
        },
    )
}

#[cfg(target_os = "macos")]
#[test]
fn different_ipc_version_restarts_the_daemon_without_replacing_current_history() -> Result<()> {
    isolated(
        "different_ipc_version_restarts_the_daemon_without_replacing_current_history",
        || {
            let root = config::home_dir()?;
            seed(&relay_config(&root)?)?;
            let dir = config::device_dir()?;
            let generation =
                xrun::testing::store::JobStore::open(&dir.join("daemon.db"), false)?.db_id;
            let _cleanup = StopFixture;
            let binary = std::env::var_os("XRUN_TEST_BINARY")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| {
                    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/xrun")
                });
            let helper = std::env::current_exe()?.with_file_name("xrun");
            std::fs::copy(binary, &helper)?;
            let mut old = Command::new(helper)
                .arg("daemon")
                .current_dir(&dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?;
            let deadline = Instant::now() + Duration::from_secs(5);
            let endpoint_path = dir.join("daemon-ipc.json");
            while !endpoint_path.exists() {
                anyhow::ensure!(
                    old.try_wait()?.is_none(),
                    "fixture daemon exited before readiness"
                );
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "fixture daemon did not publish IPC"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            let previous = xrun::client::services::daemon_state(&dir)?.generation;
            let mut endpoint: Value = serde_json::from_slice(&std::fs::read(&endpoint_path)?)?;
            endpoint["version"] = "previous-release".into();
            config::atomic_private_write(&endpoint_path, &serde_json::to_vec(&endpoint)?)?;
            let (_app, window) = build_app();
            assert!(invoke(&window, "activity_history", json!({"filter":"all"})).is_ok());
            assert!(old.wait()?.success());
            let state = xrun::client::services::daemon_state(&dir)?;
            assert!(state.running);
            assert_ne!(state.generation, previous);
            assert!(!xrun::client::services::daemon_upgrade_required()?);
            assert!(!xrun::client::services::daemon_upgrade_pending()?);
            assert_eq!(
                xrun::testing::store::JobStore::open(&dir.join("daemon.db"), false)?.db_id,
                generation
            );
            assert!(!dir.join("database-backups").exists());
            assert_eq!(invoke(&window, "stop", json!({})), Ok(Value::Null));
            Ok(())
        },
    )
}
