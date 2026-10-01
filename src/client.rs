use crate::{
    config::{self, Identity, PendingIdentity, ServerConfig},
    crypto, daemon, net,
    protocol::*,
    service,
};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Serialize;
use std::time::Duration;

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
fn user_name() -> String {
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
fn parse_link(link: &str) -> Result<(Vec<String>, String, String)> {
    let rest = link
        .strip_prefix("xrun://")
        .context("INVALID_LINK: expected xrun://")?;
    let (rest, token) = rest
        .split_once('#')
        .context("INVALID_LINK: missing token")?;
    let (addresses, pin) = rest
        .split_once('/')
        .context("INVALID_LINK: missing CA pin")?;
    if pin.len() != 52
        || !pin
            .bytes()
            .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
        || token.len() != 26
        || !token
            .bytes()
            .all(|b| b.is_ascii_lowercase() || matches!(b, b'2'..=b'7'))
    {
        bail!("INVALID_LINK: invalid CA pin or invitation token")
    }
    let mut urls = vec![];
    for address in addresses.split(',') {
        validate_address(address)?;
        urls.push(format!("https://{address}"))
    }
    if urls.is_empty() {
        bail!("INVALID_LINK: missing addresses")
    }
    Ok((urls, pin.into(), token.into()))
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
    let (addresses, pin, token) = parse_link(link)?;
    let attempts = net::ordered_addresses(&addresses, &pin);
    let dir = config::device_dir()?;
    let pending_path = dir.join("pending.toml");
    let existing = if dir.join("identity.toml").exists() {
        Some(Identity::load()?)
    } else {
        None
    };
    if existing
        .as_ref()
        .is_some_and(|id| crypto::ca_spki_pin(&id.ca_pem).ok().as_deref() != Some(&pin))
    {
        bail!("DEPLOYMENT_MISMATCH: already joined to another deployment")
    }
    let pending: PendingIdentity = if let Some(id) = &existing {
        PendingIdentity {
            key_pem: id.key_pem.clone(),
            ca_pem: id.ca_pem.clone(),
            pin: pin.clone(),
        }
    } else if pending_path.exists() {
        let value: PendingIdentity = config::read(&pending_path)?;
        if value.pin != pin {
            bail!("DEPLOYMENT_MISMATCH: pending pairing belongs to another deployment")
        }
        value
    } else {
        let mut ca = None;
        let mut error = None;
        for address in &attempts {
            match tokio::time::timeout(Duration::from_secs(5), crypto::discover_ca(address, &pin))
                .await
            {
                Ok(Ok(value)) => {
                    ca = Some(value);
                    break;
                }
                Ok(Err(e)) => error = Some(e),
                Err(_) => error = Some(anyhow::anyhow!("CONNECT_TIMEOUT: {address}")),
            }
        }
        let ca_pem = ca.ok_or_else(|| {
            error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: no reachable address"))
        })?;
        let (key_pem, _) = crypto::new_device_request()?;
        let value = PendingIdentity {
            key_pem,
            ca_pem,
            pin: pin.clone(),
        };
        config::write(&pending_path, &value)?;
        value
    };
    let name = name
        .or_else(|| existing.as_ref().map(|i| i.name.clone()))
        .unwrap_or_else(user_name);
    if !valid_name(&name) {
        bail!("INVALID_NAME: use [a-z][a-z0-9-]{{0,31}} and avoid command names")
    }
    let client = crypto::http_client(&pending.ca_pem, None)?;
    let csr = crypto::renew_device_request(&pending.key_pem)?;
    let body = PairRequest {
        token,
        name,
        csr_base64: STANDARD.encode(csr),
    };
    let mut error = None;
    for address in &attempts {
        match tokio::time::timeout(
            Duration::from_secs(5),
            client
                .post(format!("{address}/pair"))
                .header("x-xrun-version", VERSION)
                .json(&body)
                .send(),
        )
        .await
        {
            Ok(Ok(r)) => {
                if !r.status().is_success() {
                    let text = r.text().await?;
                    if let Ok(Data::Error { code, message }) = serde_json::from_str(&text) {
                        bail!("{code}: {message}")
                    }
                    bail!("PAIR_FAILED: {text}")
                };
                let pair: PairResponse = r.json().await?;
                let id = Identity {
                    device_id: pair.device_id,
                    name: pair.name,
                    addresses: addresses.clone(),
                    ca_pem: pending.ca_pem.clone(),
                    cert_pem: pair.cert_pem,
                    key_pem: pending.key_pem.clone(),
                    registration: pair.registration,
                };
                if existing
                    .as_ref()
                    .is_some_and(|old| old.device_id != id.device_id)
                {
                    bail!("IDENTITY_MISMATCH: server changed existing identity")
                };
                id.save()?;
                net::remember(&id, address);
                if pending_path.exists() {
                    std::fs::remove_file(&pending_path)?;
                    config::sync_parent(&pending_path)?
                }
                return Ok(id);
            }
            Ok(Err(e)) => error = Some(e.into()),
            Err(_) => error = Some(anyhow::anyhow!("CONNECT_TIMEOUT: {address}")),
        }
    }
    Err(error.unwrap_or_else(|| anyhow::anyhow!("CONNECT_FAILED: pairing unavailable")))
}
