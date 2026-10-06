#![cfg(unix)]
mod common;
use anyhow::Result;
use common::*;
use std::{
    io::Write,
    os::{fd::FromRawFd, unix::fs::PermissionsExt},
    path::Path,
    process::Stdio,
};

fn service_command(home: &Path, bin: &Path, args: &[&str]) -> Result<tokio::process::Command> {
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut command = command(home, args);
    command.env("PATH", std::env::join_paths(paths)?);
    Ok(command)
}

async fn purge(home: &Path, bin: &Path, answer: &[u8]) -> Result<std::process::Output> {
    let (mut master, mut slave) = (-1, -1);
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    let mut master = unsafe { std::fs::File::from_raw_fd(master) };
    let slave = unsafe { std::fs::File::from_raw_fd(slave) };
    let child = service_command(home, bin, &["down", "--purge"])?
        .stdin(Stdio::from(slave))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    master.write_all(answer)?;
    Ok(
        tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
            .await??,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_registration_removal_and_interactive_purge_preserve_identity_until_confirmed()
-> Result<()> {
    let mut lab = Lab::new().await?;
    let bin = lab.root.path().join("service-tools");
    std::fs::create_dir(&bin)?;
    // Only service-manager executables are replaced; all CLI, network and storage paths are real.
    for name in ["launchctl", "loginctl", "systemctl"] {
        let path = bin.join(name);
        std::fs::write(
            &path,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HOME/service-calls\"\nif [ \"$1\" = print ]; then exit 1; fi\n",
        )?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let home = lab.root.path().join("service-home");
    std::fs::create_dir(&home)?;
    let link = xrun::testing::relay::deployment_link(&lab.relay.config)?;
    ok(
        service_command(&home, &bin, &["up", "--relay", &link, "--name", "managed1"])?
            .output()
            .await?,
    );
    let identity = home.join(".xrun/identity.toml");
    let before = std::fs::read(&identity)?;
    assert!(
        std::fs::read_to_string(home.join("service-calls"))?.contains(
            if cfg!(target_os = "macos") {
                "bootstrap"
            } else {
                "enable"
            }
        )
    );
    for args in [
        ["daemon", "uninstall"],
        ["daemon", "install"],
        ["daemon", "start"],
    ] {
        ok(service_command(&home, &bin, &args)?.output().await?);
        assert_eq!(std::fs::read(&identity)?, before);
    }
    ok(service_command(&home, &bin, &["down"])?.output().await?);
    assert_eq!(std::fs::read(&identity)?, before);
    let rejected = service_command(&home, &bin, &["down", "--purge", "--json"])?
        .stdin(Stdio::null())
        .output()
        .await?;
    assert_eq!(rejected.status.code(), Some(125));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&rejected.stderr)?["code"],
        "INTERACTIVE_REQUIRED"
    );
    assert_eq!(std::fs::read(&identity)?, before);
    ok(purge(&home, &bin, b"no\n").await?);
    assert_eq!(std::fs::read(&identity)?, before);
    ok(purge(&home, &bin, b"purge\n").await?);
    assert!(!home.join(".xrun").exists());

    let invitation = ok(cli(&lab.source, &["invite"]).await);
    ok(service_command(
        &home,
        &bin,
        &["join", invitation.trim(), "--name", "joined1"],
    )?
    .output()
    .await?);
    let joined: xrun::testing::config::Identity = xrun::testing::config::read(&identity)?;
    assert_eq!(joined.name, "joined1");
    assert_eq!(joined.network, lab.source_identity.network);
    assert_eq!(
        xrun::testing::config::read::<xrun::testing::config::DaemonConfig>(
            &home.join(".xrun/daemon.toml")
        )?
        .allow_from,
        Vec::<String>::new()
    );
    stop_daemon(&lab.target, &mut lab.daemon).await?;
    stop_daemon(&lab.source, &mut lab.source_daemon).await?;
    Ok(())
}
