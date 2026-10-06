#![cfg(target_os = "linux")]
mod common;
use anyhow::{Context, Result};
use common::*;
use std::{os::unix::fs::PermissionsExt, path::Path, time::Duration};
use xrun::testing::config::{self, ServerConfig};

struct Deployment {
    home: tempfile::TempDir,
    path: std::ffi::OsString,
}
impl Deployment {
    fn new() -> Result<Self> {
        let home = tempfile::tempdir()?;
        let bin = home.path().join("bin");
        std::fs::create_dir(&bin)?;
        // Deployment writes real config/certificates and runs a real relay;
        // only host-wide service-manager actions are intercepted in this test.
        for name in ["systemctl", "loginctl"] {
            let exe = bin.join(name);
            std::fs::write(
                &exe,
                r#"#!/bin/sh
printf '%s' "${0##*/}" >> "$HOME/service-calls"
for arg do printf '\t%s' "$arg" >> "$HOME/service-calls"; done
printf '\n' >> "$HOME/service-calls"
if [ -f "$HOME/fail-service" ]; then printf 'injected service failure\n' >&2; exit 1; fi
"#,
            )?;
            std::fs::set_permissions(exe, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut paths = vec![bin];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        Ok(Self {
            home,
            path: std::env::join_paths(paths)?,
        })
    }
    async fn cli(&self, args: &[&str]) -> Result<std::process::Output> {
        Ok(tokio::time::timeout(
            Duration::from_secs(15),
            command(self.home.path(), args)
                .env("PATH", &self.path)
                .output(),
        )
        .await??)
    }
    fn config(&self) -> Result<ServerConfig> {
        config::read(&self.home.path().join(".xrun/config.toml"))
    }
    fn calls(&self) -> Result<String> {
        Ok(std::fs::read_to_string(
            self.home.path().join("service-calls"),
        )?)
    }
}

fn port() -> Result<u16> {
    Ok(std::net::TcpListener::bind("127.0.0.1:0")?
        .local_addr()?
        .port())
}
async fn ready(home: &Path, child: &mut tokio::process::Child) -> Result<ServerConfig> {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            anyhow::ensure!(child.try_wait()?.is_none(), "relay exited during startup");
            let cfg: ServerConfig = config::read(&home.join(".xrun/config.toml"))?;
            if let Ok(ca) = std::fs::read(cfg.data_dir.join("ca.pem")) {
                let client = reqwest::Client::builder()
                    .no_proxy()
                    .add_root_certificate(reqwest::Certificate::from_pem(&ca)?)
                    .timeout(Duration::from_millis(250))
                    .build()?;
                if let Some(address) = cfg.addresses.first()
                    && client
                        .get(format!("https://{address}/"))
                        .send()
                        .await
                        .is_ok()
                {
                    return Ok(cfg);
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?
}
async fn stop(child: &mut tokio::process::Child) -> Result<()> {
    let pid = child.id().context("relay already exited")?;
    anyhow::ensure!(unsafe { libc::kill(pid as i32, libc::SIGTERM) } == 0);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await??
            .success()
    );
    Ok(())
}

#[tokio::test]
async fn deployment_validates_before_writing_and_preserves_manual_addresses_and_keys() -> Result<()>
{
    let d = Deployment::new()?;
    for (args, code) in [
        (vec!["relay", "install", "--port", "0"], "INVALID_PORT"),
        (
            vec!["relay", "install", "--addr", "host:0"],
            "INVALID_ADDRESS",
        ),
        (
            vec!["relay", "install", "--addr", "[::1]:1234"],
            "INVALID_ADDRESS",
        ),
    ] {
        let out = d.cli(&args).await?;
        assert_eq!(out.status.code(), Some(125));
        assert!(String::from_utf8_lossy(&out.stderr).contains(code));
        assert!(!d.home.path().join(".xrun/config.toml").exists());
        assert!(!d.home.path().join("service-calls").exists());
    }
    let first_port = port()?.to_string();
    let address = format!("127.0.0.1:{first_port}");
    let first = json(
        d.cli(&[
            "relay",
            "install",
            "--port",
            &first_port,
            "--addr",
            &address,
            "--no-detect",
            "--json",
        ])
        .await?,
    );
    let cfg = d.config()?;
    assert!(cfg.manual);
    assert_eq!(cfg.addresses, [address]);
    let ca = std::fs::read(cfg.data_dir.join("ca.pem"))?;
    let key = std::fs::read(cfg.data_dir.join("ca.key"))?;
    assert!(!d.calls()?.contains("\trestart\t"));
    assert_eq!(
        json(d.cli(&["relay", "invite", "--json"]).await?)["link"],
        first["link"]
    );
    assert_eq!(
        json(
            d.cli(&["relay", "install", "--no-detect", "--json"])
                .await?
        )["link"],
        first["link"]
    );
    assert_eq!(d.config()?.addresses, cfg.addresses);
    assert!(
        !d.calls()?.contains("\trestart\t"),
        "unchanged deployment restarted the service"
    );

    let mut relay = logged(d.home.path(), &["relay", "run"], "deployed-relay")?.spawn()?;
    assert_eq!(
        ready(d.home.path(), &mut relay).await?.addresses,
        cfg.addresses
    );
    let next_port = port()?.to_string();
    stop(&mut relay).await?;
    let next_addr = format!("127.0.0.1:{next_port}");
    let changed = json(
        d.cli(&[
            "relay", "install", "--port", &next_port, "--addr", &next_addr, "--json",
        ])
        .await?,
    );
    assert_ne!(changed["link"], first["link"]);
    assert_eq!(
        d.calls()?
            .matches("systemctl\t--user\trestart\txrun-server.service")
            .count(),
        1
    );
    assert_eq!(std::fs::read(cfg.data_dir.join("ca.pem"))?, ca);
    assert_eq!(std::fs::read(cfg.data_dir.join("ca.key"))?, key);
    let mut relay = logged(d.home.path(), &["server"], "restarted-relay")?.spawn()?;
    assert_eq!(
        ready(d.home.path(), &mut relay).await?.addresses,
        [next_addr]
    );
    stop(&mut relay).await?;
    ok(d.cli(&["relay", "uninstall"]).await?);
    let calls = d.calls()?;
    ok(d.cli(&["relay", "uninstall"]).await?);
    assert_eq!(d.calls()?, calls);
    assert!(
        !d.home
            .path()
            .join(".config/systemd/user/xrun-server.service")
            .exists()
    );
    assert_eq!(std::fs::read(cfg.data_dir.join("ca.key"))?, key);
    Ok(())
}

#[tokio::test]
async fn automatic_addresses_refresh_on_run_and_failed_install_can_be_retried() -> Result<()> {
    let d = Deployment::new()?;
    let port = port()?.to_string();
    std::fs::write(d.home.path().join("fail-service"), b"")?;
    let failed = d
        .cli(&["relay", "install", "--port", &port, "--no-detect", "--json"])
        .await?;
    assert_eq!(failed.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&failed.stderr).contains("SERVICE_FAILED"));
    let mut cfg = d.config()?;
    assert!(!cfg.manual && cfg.no_detect);
    assert!(!cfg.addresses.is_empty());
    let ca = std::fs::read(cfg.data_dir.join("ca.pem"))?;
    std::fs::remove_file(d.home.path().join("fail-service"))?;
    let out = ok(d.cli(&["relay", "install"]).await?);
    assert!(out.contains("xrun up --relay 'xrun-relay://"));
    assert_eq!(std::fs::read(cfg.data_dir.join("ca.pem"))?, ca);
    cfg.addresses = vec!["stale.invalid:1".into()];
    config::write(&d.home.path().join(".xrun/config.toml"), &cfg)?;
    let mut relay = logged(d.home.path(), &["relay", "run"], "auto-relay")?.spawn()?;
    let updated = ready(d.home.path(), &mut relay).await?;
    assert!(!updated.manual && updated.no_detect);
    assert_ne!(updated.addresses, cfg.addresses);
    assert!(
        updated
            .addresses
            .iter()
            .all(|a| a.ends_with(&format!(":{port}")))
    );
    let mut unique = updated.addresses.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(updated.addresses, unique);
    stop(&mut relay).await?;
    Ok(())
}
