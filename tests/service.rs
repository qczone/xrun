#![cfg(target_os = "linux")]
mod common;
use anyhow::{Context, Result};
use common::*;
use std::{path::Path, time::Duration};
use xrun::testing::{config, protocol::VERSION};

async fn registered_cli(binary: &Path, args: &[&str]) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(binary)
            .args(args)
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    anyhow::ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

async fn connected(dir: &Path) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if config::instance_running(&dir.join("daemon.lock"))?
                && xrun::testing::control::state(dir)?.is_some_and(|s| s.connected)
            {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an empty disposable user with a running systemd user manager; set XRUN_TEST_SYSTEM_SERVICE=1"]
async fn systemd_install_stop_restart_upgrade_and_uninstall() -> Result<()> {
    anyhow::ensure!(
        std::env::var("XRUN_TEST_SYSTEM_SERVICE").as_deref() == Ok("1"),
        "real service test requires explicit opt-in"
    );
    let home = config::home_dir()?;
    let data = config::device_dir()?;
    let unit = home.join(".config/systemd/user/xrun-daemon.service");
    anyhow::ensure!(
        !data.exists() && !unit.exists(),
        "refusing to touch an existing xrun installation"
    );
    let state = tokio::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            "xrun-daemon.service",
            "--property=LoadState",
            "--value",
        ])
        .output()
        .await?;
    anyhow::ensure!(
        String::from_utf8_lossy(&state.stdout).trim() == "not-found",
        "user manager unavailable or service already registered"
    );
    let temp = tempfile::tempdir()?;
    struct Cleanup {
        data: std::path::PathBuf,
        unit: std::path::PathBuf,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "disable", "--now", "xrun-daemon.service"])
                .output();
            let _ = std::fs::remove_file(&self.unit);
            let _ = std::process::Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .output();
            let _ = std::fs::remove_dir_all(&self.data);
        }
    }
    let _cleanup = Cleanup {
        data: data.clone(),
        unit: unit.clone(),
    };
    let (_relay, link) = TestRelay::new(&temp.path().join("relay-home")).await?;
    let install_dir = temp.path().join("installed with spaces and % percent");
    std::fs::create_dir(&install_dir)?;
    let old = install_dir.join("xrun");
    let upgraded = install_dir.join("xrun-upgraded");
    std::fs::copy(binary(), &old)?;
    std::fs::copy(binary(), &upgraded)?;
    registered_cli(
        &old,
        &["up", "--relay", &link, "--name", "service1", "--no-daemon"],
    )
    .await?;
    let identity = std::fs::read(data.join("identity.toml"))?;
    let id = config::Identity::load()?;
    config::update_permission(&id.device_id, true)?;

    registered_cli(&old, &["daemon", "install"]).await?;
    connected(&data).await?;
    let generation = xrun::testing::control::state(&data)?.unwrap().generation;
    registered_cli(&old, &["daemon", "install"]).await?;
    assert_eq!(
        xrun::testing::control::state(&data)?.unwrap().generation,
        generation,
        "reinstall restarted a healthy daemon"
    );
    assert_eq!(
        registered_cli(
            &old,
            &["service1", "--", old.to_str().unwrap(), "--version"]
        )
        .await?
        .trim(),
        format!("xrun {VERSION}")
    );
    let job: serde_json::Value = serde_json::from_str(
        &registered_cli(
            &old,
            &["service1", "start", "--json", "--", "/bin/sleep", "120"],
        )
        .await?,
    )?;
    let job_id = job["job_id"].as_str().context("job id")?;
    let store = xrun::testing::store::TaskStore::open(&data.join("daemon.db"), false)?;
    let pid = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(process) = store.get(job_id)?.and_then(|j| j.process) {
                return Ok::<_, anyhow::Error>(process.pid);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    registered_cli(&old, &["daemon", "stop"]).await?;
    assert!(!config::instance_running(&data.join("daemon.lock"))?);
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "service stop left a job process running"
    );
    // Restart=on-failure must not undo an intentional stop after RestartSec=2.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert!(!config::instance_running(&data.join("daemon.lock"))?);
    registered_cli(&old, &["daemon", "start"]).await?;
    connected(&data).await?;
    assert_ne!(
        xrun::testing::control::state(&data)?.unwrap().generation,
        generation
    );
    registered_cli(&old, &["daemon", "stop"]).await?;
    registered_cli(&upgraded, &["daemon", "install"]).await?;
    connected(&data).await?;
    let pid = tokio::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            "xrun-daemon.service",
            "--property=MainPID",
            "--value",
        ])
        .output()
        .await?;
    let pid = String::from_utf8(pid.stdout)?.trim().parse::<u32>()?;
    assert_eq!(std::fs::read_link(format!("/proc/{pid}/exe"))?, upgraded);
    assert_eq!(std::fs::read(data.join("identity.toml"))?, identity);
    registered_cli(&upgraded, &["daemon", "uninstall"]).await?;
    registered_cli(&upgraded, &["daemon", "uninstall"]).await?;
    assert!(!unit.exists());
    assert!(!config::instance_running(&data.join("daemon.lock"))?);
    assert_eq!(
        std::fs::read(data.join("identity.toml"))?,
        identity,
        "removing a service erased membership"
    );
    Ok(())
}
