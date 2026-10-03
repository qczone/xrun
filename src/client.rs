use crate::{
    config::{self, Identity, ServerConfig},
    daemon, net,
    protocol::*,
    service,
};
use anyhow::{Context, Result, bail};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Permission {
    pub source_device_id: String,
    pub allowed: bool,
}
#[derive(Debug, Serialize)]
pub struct JoinInfo {
    pub device_id: String,
    pub name: String,
    pub registration: Registration,
}
pub async fn join(link: &str, name: Option<String>) -> Result<JoinInfo> {
    let id = join_identity(link, name).await?;
    daemon::init()?;
    Ok(JoinInfo {
        device_id: id.device_id,
        name: id.name,
        registration: id.registration,
    })
}
#[derive(Debug, Serialize)]
pub struct LocalStatus {
    pub joined: bool,
    pub device_id: Option<String>,
    pub name: Option<String>,
    pub version: &'static str,
    pub daemon_initialized: bool,
    pub daemon_installed: bool,
    pub daemon_running: bool,
    pub daemon_connected: Option<bool>,
    pub remote_access_paused: bool,
    pub allow_all: bool,
    pub server_configured: bool,
    pub server_installed: bool,
    pub server_running: bool,
}
#[derive(Debug, Serialize)]
pub struct Status {
    pub local: LocalStatus,
    pub devices: Option<Vec<Device>>,
    pub server_error: Option<Data>,
}
pub fn local_status() -> Result<LocalStatus> {
    let dir = config::device_dir()?;
    let policy = config::DaemonConfig::load()?;
    let id = if dir.join("identity.toml").exists() {
        Some(Identity::load()?)
    } else {
        None
    };
    let daemon_running = config::instance_running(&dir.join("daemon.lock"))?;
    let connected = if daemon_running {
        crate::control::state(&dir)?.map(|s| s.connected)
    } else {
        Some(false)
    };
    let server_running = if let Ok(cfg) = ServerConfig::load() {
        config::instance_running(&cfg.data_dir.join("server.lock"))?
    } else {
        false
    };
    Ok(LocalStatus {
        joined: id.is_some(),
        device_id: id.as_ref().map(|i| i.device_id.clone()),
        name: id.map(|i| i.name),
        version: VERSION,
        daemon_initialized: dir.join("daemon.initialized").exists(),
        daemon_installed: service::installed("daemon")?,
        daemon_running,
        daemon_connected: connected,
        remote_access_paused: policy.remote_access_paused,
        allow_all: policy.allow_all,
        server_configured: dir.join("config.toml").exists(),
        server_installed: service::installed("server")?,
        server_running,
    })
}
pub async fn status() -> Result<Status> {
    let local = local_status()?;
    let mut devices = None;
    let mut error = None;
    if local.joined {
        let mut id = Identity::load()?;
        match net::renew_identity(&mut id).await {
            Ok(()) => {
                match net::http::<Vec<Device>>(&id, reqwest::Method::GET, "/devices", None).await {
                    Ok(values) => devices = Some(values),
                    Err(e) => error = Some(Data::error(&e)),
                }
            }
            Err(e) => error = Some(Data::error(&e)),
        }
    }
    Ok(Status {
        local,
        devices,
        server_error: error,
    })
}
pub(crate) fn user_name() -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| std::env::consts::OS.into());
    let mut value: String = host
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    if value.is_empty() || !value.as_bytes()[0].is_ascii_lowercase() || !valid_name(&value) {
        value = format!("{}1", std::env::consts::OS)
    }
    value
}
pub async fn set_permission(value: &str, allow: bool) -> Result<Permission> {
    let id = Identity::load()?;
    let device_id = if let Some(key) = value.strip_prefix("dev_") {
        if key.len() != 32 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("INVALID_DEVICE_ID: expected dev_ followed by 32 hexadecimal digits");
        }
        value.to_string()
    } else {
        let device: Device = net::http(
            &id,
            reqwest::Method::GET,
            &format!("/devices/{value}"),
            None,
        )
        .await?;
        device.device_id
    };
    config::update_permission(&device_id, allow)?;
    Ok(Permission {
        source_device_id: device_id,
        allowed: allow,
    })
}
pub(crate) fn validate_address(address: &str) -> Result<()> {
    let (_, port) = address
        .rsplit_once(':')
        .context("INVALID_ADDRESS: an explicit port is required")?;
    if port.parse::<u16>().ok().is_none_or(|p| p == 0) {
        bail!("INVALID_ADDRESS: invalid port")
    }
    let url = url::Url::parse(&format!("https://{address}"))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || matches!(url.host(), Some(url::Host::Ipv6(_)))
    {
        bail!("INVALID_ADDRESS: expected IPv4 or hostname followed by :port")
    };
    Ok(())
}
pub(crate) async fn join_identity(link: &str, name: Option<String>) -> Result<Identity> {
    let name = name
        .or_else(|| Identity::load().ok().map(|id| id.name))
        .unwrap_or_else(user_name);
    crate::network::join(link, name).await
}
