use super::*;
use std::{os::unix::fs::PermissionsExt, process::Stdio, time::Instant};

fn isolated(name: &str, test: impl FnOnce() -> Result<()>) -> Result<()> {
    if std::env::var("XRUN_SERVICE_TEST").as_deref() == Ok(name) {
        return test();
    }
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home & spaces");
    let bin = temp.path().join("bin");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&bin)?;
    // Intercept only service-manager utilities. No real registration is touched.
    for name in ["launchctl", "systemctl", "loginctl"] {
        let path = bin.join(name);
        std::fs::write(
            &path,
            r#"#!/bin/sh
name=${0##*/}
printf '%s' "$name" >> "$HOME/service-calls"
for arg do printf '\t%s' "$arg" >> "$HOME/service-calls"; done
printf '\n' >> "$HOME/service-calls"
action=$1
if [ "$name" = systemctl ]; then action=$2; fi
if [ -f "$HOME/fail-$name-$action" ]; then
  printf 'injected service-manager failure\n' >&2
  exit 9
fi
case "$name/$action" in
  launchctl/print) test -f "$HOME/service-active"; exit $?;;
  launchctl/bootstrap) : > "$HOME/service-active";;
  launchctl/bootout) /bin/rm -f "$HOME/service-active";;
esac
"#,
        )?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let mut child = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", &format!("service::tests::{name}"), "--nocapture"])
        .env("XRUN_SERVICE_TEST", name)
        .env("HOME", home)
        .env("PATH", std::env::join_paths(paths)?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait()?.is_none() {
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            bail!("service-command test timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn calls() -> Result<String> {
    Ok(std::fs::read_to_string(
        config::home_dir()?.join("service-calls"),
    )?)
}
fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

#[test]
fn registration_quotes_paths_reuses_existing_service_and_preserves_identity() -> Result<()> {
    isolated(
        "registration_quotes_paths_reuses_existing_service_and_preserves_identity",
        || {
            runtime()?.block_on(async {
                let home = config::home_dir()?;
                let identity = config::device_dir()?.join("identity.toml");
                config::atomic_private_write(&identity, b"preserved identity")?;
                let exe = home.join("tool & <special> \"quoted\" % name\nxrun");
                install_with_executable("daemon", &exe).await?;
                assert!(installed("daemon")?);
                let path = unit_path("daemon")?;
                assert_eq!(
                    std::fs::metadata(&path)?.permissions().mode() & 0o777,
                    0o600
                );
                #[cfg(target_os = "macos")]
                {
                    let parsed = tokio::process::Command::new("/usr/bin/plutil")
                        .args(["-convert", "json", "-o", "-"])
                        .arg(&path)
                        .output()
                        .await?;
                    anyhow::ensure!(parsed.status.success(), "invalid launchd plist");
                    let value: serde_json::Value = serde_json::from_slice(&parsed.stdout)?;
                    assert_eq!(
                        value["ProgramArguments"],
                        serde_json::json!([exe, "daemon"])
                    );
                    assert_eq!(value["KeepAlive"]["SuccessfulExit"], false);
                    assert_eq!(
                        value["StandardErrorPath"],
                        config::device_dir()?
                            .join("daemon-service.log")
                            .to_string_lossy()
                            .as_ref()
                    );
                    let before = calls()?;
                    install_with_executable("daemon", &exe).await?;
                    assert_eq!(
                        calls()?.matches("\tbootstrap\t").count(),
                        before.matches("\tbootstrap\t").count()
                    );
                    assert!(cli_daemon_installed()?);
                    let error = install_with_executable("server", &exe).await.unwrap_err();
                    assert!(crate::error::is(&error, ErrorCode::UnsupportedPlatform));
                    assert!(restart("server").await.is_err());
                }
                #[cfg(target_os = "linux")]
                {
                    let unit = std::fs::read_to_string(&path)?;
                    assert!(unit.contains("\\\"quoted\\\" %% name\\nxrun\" daemon\n"));
                    assert!(unit.contains("Restart=on-failure\n"));
                    install_with_executable("daemon", &exe).await?;
                    assert!(!calls()?.contains("\trestart\t"));
                    restart("daemon").await?;
                    assert!(calls()?.contains("systemctl\t--user\trestart\txrun-daemon.service"));
                    assert!(crate::error::is(
                        &install_with_executable("invalid", &exe).await.unwrap_err(),
                        ErrorCode::InvalidService
                    ));
                }
                start("daemon").await?;
                uninstall("daemon").await?;
                assert!(!path.exists());
                let before = calls()?;
                uninstall("daemon").await?;
                assert_eq!(
                    calls()?,
                    before,
                    "repeated removal contacted the service manager"
                );
                assert_eq!(std::fs::read(identity)?, b"preserved identity");
                Ok(())
            })
        },
    )
}

#[test]
fn service_manager_failures_are_reported_and_registration_can_be_retried() -> Result<()> {
    isolated(
        "service_manager_failures_are_reported_and_registration_can_be_retried",
        || {
            runtime()?.block_on(async {
                assert!(crate::error::is(
                    &start("uninstalled").await.unwrap_err(),
                    ErrorCode::ServiceNotInstalled
                ));
                assert!(crate::error::is(
                    &command("/nonexistent-xrun-service-test-command", &[])
                        .await
                        .unwrap_err(),
                    ErrorCode::ServiceUnavailable
                ));
                let home = config::home_dir()?;
                let exe = home.join("helper");
                let fail = home.join(if cfg!(target_os = "macos") {
                    "fail-launchctl-bootstrap"
                } else {
                    "fail-loginctl-enable-linger"
                });
                std::fs::write(&fail, b"")?;
                let error = install_with_executable("daemon", &exe).await.unwrap_err();
                assert!(
                    crate::error::is(&error, ErrorCode::ServiceFailed),
                    "{error:#}"
                );
                assert!(
                    unit_path("daemon")?.exists(),
                    "retry configuration was lost"
                );
                assert!(!calls()?.contains("\tkickstart\t"));
                assert!(!calls()?.contains("\tenable\t"));
                std::fs::remove_file(fail)?;
                install_with_executable("daemon", &exe).await?;
                let fail = home.join(if cfg!(target_os = "macos") {
                    "fail-launchctl-kickstart"
                } else {
                    "fail-systemctl-start"
                });
                std::fs::write(&fail, b"")?;
                assert!(crate::error::is(
                    &start("daemon").await.unwrap_err(),
                    ErrorCode::ServiceFailed
                ));
                assert!(installed("daemon")?);
                std::fs::remove_file(fail)?;
                start("daemon").await?;
                #[cfg(target_os = "linux")]
                {
                    let fail = home.join("fail-systemctl-disable");
                    std::fs::write(&fail, b"")?;
                    assert!(uninstall("daemon").await.is_err());
                    assert!(installed("daemon")?, "failed removal discarded the unit");
                    std::fs::remove_file(fail)?;
                }
                uninstall("daemon").await?;
                Ok(())
            })
        },
    )
}
