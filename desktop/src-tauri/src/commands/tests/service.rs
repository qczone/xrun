//! Service replacement keeps current-format history and resumes a running daemon.
use super::*;

#[cfg(target_os = "macos")]
struct StopFixture;
#[cfg(target_os = "macos")]
impl Drop for StopFixture {
    fn drop(&mut self) {
        let _ = tauri::async_runtime::block_on(xrun::client::services::stop_daemon());
    }
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
            let helper_error = dir.join("helper-error.log");
            let mut old = Command::new(helper)
                .arg("daemon")
                .current_dir(&dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(&helper_error)?)
                .spawn()?;
            let deadline = Instant::now() + Duration::from_secs(5);
            let endpoint_path = dir.join("daemon-ipc.json");
            while !endpoint_path.exists() {
                let exited = old.try_wait()?;
                anyhow::ensure!(
                    exited.is_none(),
                    "fixture daemon exited before readiness: {exited:?}; {}",
                    std::fs::read_to_string(&helper_error)?
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
            let history = invoke(&window, "activity_history", json!({"filter":"all"}));
            assert!(history.is_ok(), "{history:?}");
            let exited = old.wait()?;
            anyhow::ensure!(
                exited.success(),
                "old daemon stopped with {exited}; {}",
                std::fs::read_to_string(&helper_error)?
            );
            let state = xrun::client::services::daemon_state(&dir)?;
            assert!(state.running);
            assert_ne!(state.generation, previous);
            assert!(!xrun::client::services::daemon_restart_required()?);
            assert!(!xrun::client::services::daemon_restart_pending()?);
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

#[cfg(target_os = "macos")]
#[test]
fn different_ipc_protocol_restarts_the_daemon_without_replacing_current_history() -> Result<()> {
    isolated(
        "different_ipc_protocol_restarts_the_daemon_without_replacing_current_history",
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
            let helper_error = dir.join("helper-error.log");
            let mut old = Command::new(helper)
                .arg("daemon")
                .current_dir(&dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(&helper_error)?)
                .spawn()?;
            let deadline = Instant::now() + Duration::from_secs(5);
            let endpoint_path = dir.join("daemon-ipc.json");
            while !endpoint_path.exists() {
                let exited = old.try_wait()?;
                anyhow::ensure!(
                    exited.is_none(),
                    "fixture daemon exited before readiness: {exited:?}; {}",
                    std::fs::read_to_string(&helper_error)?
                );
                anyhow::ensure!(
                    Instant::now() < deadline,
                    "fixture daemon did not publish IPC"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            let previous = xrun::client::services::daemon_state(&dir)?.generation;
            let mut endpoint: Value = serde_json::from_slice(&std::fs::read(&endpoint_path)?)?;
            endpoint["protocol"] = 0.into();
            config::atomic_private_write(&endpoint_path, &serde_json::to_vec(&endpoint)?)?;
            let (_app, window) = build_app();
            let history = invoke(&window, "activity_history", json!({"filter":"all"}));
            assert!(history.is_ok(), "{history:?}");
            let exited = old.wait()?;
            anyhow::ensure!(
                exited.success(),
                "old daemon stopped with {exited}; {}",
                std::fs::read_to_string(&helper_error)?
            );
            let state = xrun::client::services::daemon_state(&dir)?;
            assert!(state.running);
            assert_ne!(state.generation, previous);
            assert!(!xrun::client::services::daemon_restart_required()?);
            assert!(!xrun::client::services::daemon_restart_pending()?);
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
