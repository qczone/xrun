use crate::config;
#[cfg(not(target_os = "linux"))]
use crate::config::device_dir;
use anyhow::{Context, Result, bail};
#[cfg(unix)]
use std::path::PathBuf;

async fn command(program: &str, args: &[&str]) -> Result<()> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("SERVICE_UNAVAILABLE: {program}"))?;
    if !output.status.success() {
        bail!(
            "SERVICE_FAILED: {program}: {}",
            String::from_utf8_lossy(&output.stderr)
        )
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
    if !["server", "daemon"].contains(&kind) {
        bail!("INVALID_SERVICE: {kind}")
    }
    let path = unit_path(kind)?;
    let exe = std::env::current_exe()?;
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
    if kind != "daemon" {
        bail!("UNSUPPORTED_PLATFORM: Server requires Linux")
    }
    let path = unit_path(kind)?;
    let exe = std::env::current_exe()?;
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
    if kind != "daemon" {
        bail!("UNSUPPORTED_PLATFORM: Server requires Linux")
    }
    let script = r#"$u=[Security.Principal.WindowsIdentity]::GetCurrent().Name; $a=New-ScheduledTaskAction -Execute $env:XRUN_SERVICE_EXE -Argument daemon; $t=New-ScheduledTaskTrigger -AtLogOn -User $u; $p=New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Limited; $s=New-ScheduledTaskSettingsSet -RestartCount 999 -RestartInterval (New-TimeSpan -Minutes 1) -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries; Register-ScheduledTask -TaskName xrun-daemon -Action $a -Trigger $t -Principal $p -Settings $s -Force | Out-Null; Start-ScheduledTask -TaskName xrun-daemon"#;
    let status = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("XRUN_SERVICE_EXE", std::env::current_exe()?)
        .status()
        .await?;
    if !status.success() {
        bail!("SERVICE_FAILED: scheduled task installation failed")
    }
    config::atomic_private_write(&device_dir()?.join("daemon.service"), b"scheduled-task\n")?;
    Ok(())
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
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        Ok(unit_path(kind)?.exists())
    }
    #[cfg(windows)]
    {
        Ok(kind == "daemon" && device_dir()?.join("daemon.service").exists())
    }
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
    bail!("UNSUPPORTED_PLATFORM: Server requires Linux")
}
