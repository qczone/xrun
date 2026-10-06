use crate::config;
#[cfg(not(target_os = "linux"))]
use crate::config::device_dir;
use crate::error::ErrorCode;
use anyhow::{Context, Result, bail};
#[cfg(unix)]
use std::path::PathBuf;
use std::{path::Path, time::Duration};

#[cfg(target_os = "macos")]
pub(crate) const APP_DAEMON_LABEL: &str = "dev.qczone.xrun.daemon";

async fn command(program: &str, args: &[&str]) -> Result<()> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(args);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW for service-manager utilities.
    let output = cmd
        .output()
        .await
        .with_context(|| ErrorCode::ServiceUnavailable.error(program.to_string()))?;
    if !output.status.success() {
        bail!(ErrorCode::ServiceFailed.error(format!(
            "{program}: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
    Ok(())
}
#[cfg(target_os = "linux")]
fn unit_path(kind: &str) -> Result<PathBuf> {
    Ok(config::home_dir()?
        .join(".config/systemd/user")
        .join(format!("xrun-{kind}.service")))
}
#[cfg(target_os = "linux")]
fn systemd_quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('\n', "\\n")
    )
}
#[cfg(target_os = "linux")]
pub(crate) async fn install(kind: &str) -> Result<()> {
    install_with_executable(kind, &std::env::current_exe()?).await
}

/// The desktop application must register the bundled CLI, never its own executable.
#[cfg(target_os = "linux")]
pub(crate) async fn install_with_executable(kind: &str, exe: &Path) -> Result<()> {
    if !["server", "daemon"].contains(&kind) {
        bail!(ErrorCode::InvalidService.error(kind.to_string()))
    }
    let path = unit_path(kind)?;
    let text = format!(
        concat!(
            "[Unit]\nDescription=xrun {kind}\nAfter=network-online.target\n\n",
            "[Service]\nExecStart={} {kind}\nRestart=on-failure\nRestartSec=2\nTimeoutStopSec=12\n\n",
            "[Install]\nWantedBy=default.target\n"
        ),
        systemd_quote(&exe.to_string_lossy()),
        kind = kind,
    );
    config::atomic_private_write(&path, text.as_bytes())?;
    let user = tokio::process::Command::new("id")
        .arg("-un")
        .output()
        .await?;
    let user = String::from_utf8(user.stdout)?;
    command("loginctl", &["enable-linger", user.trim()])
        .await
        .with_context(|| format!("enable linger with: loginctl enable-linger {}", user.trim()))?;
    command("systemctl", &["--user", "daemon-reload"]).await?;
    command(
        "systemctl",
        &["--user", "enable", "--now", &format!("xrun-{kind}.service")],
    )
    .await
}
#[cfg(target_os = "linux")]
pub(crate) async fn uninstall(kind: &str) -> Result<()> {
    let path = unit_path(kind)?;
    if path.exists() {
        command(
            "systemctl",
            &[
                "--user",
                "disable",
                "--now",
                &format!("xrun-{kind}.service"),
            ],
        )
        .await?;
        std::fs::remove_file(path)?;
        command("systemctl", &["--user", "daemon-reload"]).await?;
    }
    Ok(())
}
#[cfg(target_os = "macos")]
fn unit_path(kind: &str) -> Result<PathBuf> {
    Ok(config::home_dir()?
        .join("Library/LaunchAgents")
        .join(format!("com.xrun.{kind}.plist")))
}
#[cfg(target_os = "macos")]
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
#[cfg(target_os = "macos")]
pub(crate) async fn install(kind: &str) -> Result<()> {
    install_with_executable(kind, &std::env::current_exe()?).await
}
#[cfg(target_os = "macos")]
pub(crate) async fn install_with_executable(kind: &str, exe: &Path) -> Result<()> {
    if kind != "daemon" {
        bail!(ErrorCode::UnsupportedPlatform.error("Server requires Linux"))
    }
    let path = unit_path(kind)?;
    let dir = device_dir()?;
    let log = xml(&dir.join("daemon-service.log").to_string_lossy());
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.xrun.daemon</string>
<key>ProgramArguments</key><array><string>{}</string><string>daemon</string></array>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
<key>StandardOutPath</key><string>{log}</string>
<key>StandardErrorPath</key><string>{log}</string></dict></plist>"#,
        xml(&exe.to_string_lossy())
    );
    config::atomic_private_write(&path, text.as_bytes())?;
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    if tokio::process::Command::new("launchctl")
        .args(["print", &format!("{domain}/com.xrun.daemon")])
        .output()
        .await?
        .status
        .success()
    {
        return Ok(());
    }
    let _ = command(
        "launchctl",
        &["bootout", &format!("{domain}/com.xrun.daemon")],
    )
    .await;
    command(
        "launchctl",
        &["bootstrap", &domain, &path.to_string_lossy()],
    )
    .await?;
    command(
        "launchctl",
        &["kickstart", &format!("{domain}/com.xrun.daemon")],
    )
    .await
}
#[cfg(target_os = "macos")]
pub(crate) async fn uninstall(kind: &str) -> Result<()> {
    let path = unit_path(kind)?;
    if path.exists() {
        let domain = format!("gui/{}/com.xrun.{kind}", unsafe { libc::getuid() });
        let _ = command("launchctl", &["bootout", &domain]).await;
        std::fs::remove_file(path)?;
    }
    Ok(())
}
#[cfg(windows)]
pub(crate) async fn install(kind: &str) -> Result<()> {
    install_with_executable(kind, &std::env::current_exe()?).await
}
#[cfg(windows)]
pub(crate) async fn install_with_executable(kind: &str, exe: &Path) -> Result<()> {
    if kind != "daemon" {
        bail!(ErrorCode::UnsupportedPlatform.error("Server requires Linux"))
    }
    let script = r#"
$user = [Security.Principal.WindowsIdentity]::GetCurrent().Name
$action = New-ScheduledTaskAction -Execute $env:XRUN_SERVICE_EXE -Argument daemon
$trigger = New-ScheduledTaskTrigger -AtLogOn -User $user
$principal = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
$settings = New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) `
    -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName xrun-daemon -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
Start-ScheduledTask -TaskName xrun-daemon
"#;
    let output = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("XRUN_SERVICE_EXE", exe)
        .creation_flags(0x08000000)
        .output()
        .await?;
    if !output.status.success() {
        bail!(
            ErrorCode::ServiceFailed.error(format!("{}", String::from_utf8_lossy(&output.stderr)))
        )
    }
    config::atomic_private_write(&device_dir()?.join("daemon.service"), b"scheduled-task\n")?;
    Ok(())
}

pub(crate) async fn start(kind: &str) -> Result<()> {
    if !installed(kind)? {
        bail!(ErrorCode::ServiceNotInstalled.error(kind.to_string()))
    }
    #[cfg(target_os = "linux")]
    return command(
        "systemctl",
        &["--user", "start", &format!("xrun-{kind}.service")],
    )
    .await;
    #[cfg(target_os = "macos")]
    {
        let domain = format!("gui/{}", unsafe { libc::getuid() });
        if kind == "daemon" && !cli_daemon_installed()? {
            return command(
                "/bin/launchctl",
                &["kickstart", &format!("{domain}/{APP_DAEMON_LABEL}")],
            )
            .await;
        }
        let _ = command(
            "launchctl",
            &["bootstrap", &domain, &unit_path(kind)?.to_string_lossy()],
        )
        .await;
        command(
            "launchctl",
            &["kickstart", &format!("{domain}/com.xrun.{kind}")],
        )
        .await
    }
    #[cfg(windows)]
    command("schtasks", &["/Run", "/TN", &format!("xrun-{kind}")]).await
}

/// A successful daemon exit is deliberately not restarted by its supervisor.
pub(crate) async fn stop_daemon() -> Result<()> {
    let dir = config::device_dir()?;
    if !config::instance_running(&dir.join("daemon.lock"))? {
        return Ok(());
    }
    if crate::control::state(&dir)?.is_some() {
        crate::control::request_shutdown(&dir).await?;
    } else {
        // Older daemons do not have the private stop channel. Unix SIGTERM still
        // follows the same cleanup path; Windows task termination does not.
        #[cfg(target_os = "macos")]
        command(
            "launchctl",
            &[
                "kill",
                "SIGTERM",
                &format!("gui/{}/com.xrun.daemon", unsafe { libc::getuid() }),
            ],
        )
        .await?;
        #[cfg(target_os = "linux")]
        command("systemctl", &["--user", "stop", "xrun-daemon.service"]).await?;
        #[cfg(windows)]
        bail!(
            ErrorCode::DaemonUpgradeRequired
                .error("stop the old daemon before upgrading to the desktop app")
        );
    }
    tokio::time::timeout(Duration::from_secs(12), async {
        while config::instance_running(&dir.join("daemon.lock"))? {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context(ErrorCode::DaemonStopTimeout.error("daemon did not finish cleanup"))?
}
#[cfg(windows)]
pub(crate) async fn uninstall(kind: &str) -> Result<()> {
    if kind == "daemon" && device_dir()?.join("daemon.service").exists() {
        let _ = command("schtasks", &["/End", "/TN", "xrun-daemon"]).await;
        command("schtasks", &["/Delete", "/TN", "xrun-daemon", "/F"]).await?;
        std::fs::remove_file(device_dir()?.join("daemon.service"))?;
    }
    Ok(())
}
pub(crate) fn installed(kind: &str) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        Ok(unit_path(kind)?.exists())
    }
    #[cfg(target_os = "macos")]
    {
        if unit_path(kind)?.exists() {
            return Ok(true);
        }
        if kind != "daemon" {
            return Ok(false);
        }
        let domain = format!("gui/{}/{APP_DAEMON_LABEL}", unsafe { libc::getuid() });
        Ok(std::process::Command::new("/bin/launchctl")
            .args(["print", &domain])
            .output()?
            .status
            .success())
    }
    #[cfg(windows)]
    {
        Ok(kind == "daemon" && device_dir()?.join("daemon.service").exists())
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn cli_daemon_installed() -> Result<bool> {
    Ok(unit_path("daemon")?.exists())
}

#[cfg(target_os = "linux")]
pub(crate) async fn restart(kind: &str) -> Result<()> {
    command(
        "systemctl",
        &["--user", "restart", &format!("xrun-{kind}.service")],
    )
    .await
}
#[cfg(not(target_os = "linux"))]
pub(crate) async fn restart(_kind: &str) -> Result<()> {
    bail!(ErrorCode::UnsupportedPlatform.error("Server requires Linux"))
}

#[cfg(all(test, unix))]
mod tests {
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
                        assert!(
                            calls()?.contains("systemctl\t--user\trestart\txrun-daemon.service")
                        );
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
}
