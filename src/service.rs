use crate::config;
#[cfg(not(target_os = "linux"))]
use crate::config::device_dir;
use crate::error::ErrorCode;
use anyhow::{Context, Result, bail};
#[cfg(unix)]
use std::path::PathBuf;
use std::{path::Path, time::Duration};

#[cfg(all(test, unix))]
mod tests;

#[cfg(target_os = "macos")]
pub const APP_DAEMON_LABEL: &str = "dev.qczone.xrun.daemon";

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
pub async fn install(kind: &str) -> Result<()> {
    install_with_executable(kind, &std::env::current_exe()?).await
}

/// The desktop application must register the bundled CLI, never its own executable.
#[cfg(target_os = "linux")]
pub async fn install_with_executable(kind: &str, exe: &Path) -> Result<()> {
    if !["server", "daemon"].contains(&kind) {
        bail!(ErrorCode::InvalidService.error(kind.to_string()))
    }
    let path = unit_path(kind)?;
    let text = format!(
        "[Unit]\nDescription=xrun {kind}\nAfter=network-online.target\n\n[Service]\nExecStart={} {kind}\nRestart=on-failure\nRestartSec=2\nTimeoutStopSec=12\n\n[Install]\nWantedBy=default.target\n",
        systemd_quote(&exe.to_string_lossy())
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
pub async fn uninstall(kind: &str) -> Result<()> {
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
pub async fn install(kind: &str) -> Result<()> {
    install_with_executable(kind, &std::env::current_exe()?).await
}
#[cfg(target_os = "macos")]
pub async fn install_with_executable(kind: &str, exe: &Path) -> Result<()> {
    if kind != "daemon" {
        bail!(ErrorCode::UnsupportedPlatform.error("Server requires Linux"))
    }
    let path = unit_path(kind)?;
    let dir = device_dir()?;
    let log = xml(&dir.join("daemon-service.log").to_string_lossy());
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>com.xrun.daemon</string><key>ProgramArguments</key><array><string>{}</string><string>daemon</string></array><key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>StandardOutPath</key><string>{log}</string><key>StandardErrorPath</key><string>{log}</string></dict></plist>"#,
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
pub async fn uninstall(kind: &str) -> Result<()> {
    let path = unit_path(kind)?;
    if path.exists() {
        let domain = format!("gui/{}/com.xrun.{kind}", unsafe { libc::getuid() });
        let _ = command("launchctl", &["bootout", &domain]).await;
        std::fs::remove_file(path)?;
    }
    Ok(())
}
#[cfg(windows)]
pub async fn install(kind: &str) -> Result<()> {
    install_with_executable(kind, &std::env::current_exe()?).await
}
#[cfg(windows)]
pub async fn install_with_executable(kind: &str, exe: &Path) -> Result<()> {
    if kind != "daemon" {
        bail!(ErrorCode::UnsupportedPlatform.error("Server requires Linux"))
    }
    let script = r#"$u=[Security.Principal.WindowsIdentity]::GetCurrent().Name; $a=New-ScheduledTaskAction -Execute $env:XRUN_SERVICE_EXE -Argument daemon; $t=New-ScheduledTaskTrigger -AtLogOn -User $u; $p=New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Limited; $s=New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries; Register-ScheduledTask -TaskName xrun-daemon -Action $a -Trigger $t -Principal $p -Settings $s -Force | Out-Null; Start-ScheduledTask -TaskName xrun-daemon"#;
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

pub async fn start(kind: &str) -> Result<()> {
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
pub async fn stop_daemon() -> Result<()> {
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
pub async fn uninstall(kind: &str) -> Result<()> {
    if kind == "daemon" && device_dir()?.join("daemon.service").exists() {
        let _ = command("schtasks", &["/End", "/TN", "xrun-daemon"]).await;
        command("schtasks", &["/Delete", "/TN", "xrun-daemon", "/F"]).await?;
        std::fs::remove_file(device_dir()?.join("daemon.service"))?;
    }
    Ok(())
}
pub fn installed(kind: &str) -> Result<bool> {
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
pub fn cli_daemon_installed() -> Result<bool> {
    Ok(unit_path("daemon")?.exists())
}

#[cfg(target_os = "linux")]
pub async fn restart(kind: &str) -> Result<()> {
    command(
        "systemctl",
        &["--user", "restart", &format!("xrun-{kind}.service")],
    )
    .await
}
#[cfg(not(target_os = "linux"))]
pub async fn restart(_kind: &str) -> Result<()> {
    bail!(ErrorCode::UnsupportedPlatform.error("Server requires Linux"))
}
