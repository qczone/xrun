//! Local snapshots and relay device discovery.
use crate::{
    config::{self, Identity, ServerConfig},
    net,
    protocol::*,
    service,
};
use anyhow::Result;
use serde::Serialize;
/// Locally verified network identity, available without contacting a relay.
#[derive(Serialize)]
pub struct NetworkStatus {
    /// Network identifier derived from its root public key.
    pub network_id: String,
    /// Immutable ID of the membership authority.
    pub manager_id: String,
    /// Current display name of the authority in the verified roster.
    pub manager_name: String,
    /// Whether this device owns the manager identity.
    pub is_manager: bool,
    /// Signed relay endpoints, including any private route.
    pub relay_addresses: Vec<String>,
}

/// Local state and access policy for a desktop snapshot, without relay IO.
pub struct LocalSnapshot {
    /// Identity, daemon and server summary.
    pub local: LocalStatus,
    /// Verified network information; absent when unjoined or validation fails.
    pub network: Option<NetworkStatus>,
    /// Explicitly allowed source device IDs.
    pub allow_from: Vec<String>,
    /// Explicitly denied source device IDs; these override all-member access.
    pub deny_from: Vec<String>,
    /// Network validation failure, while other local status remains available.
    pub network_error: Option<Data>,
}

/// Read identity, policy and verified roster. A roster failure is returned inside
/// `network_error` so callers can still show service controls; filesystem failures
/// reading identity or configuration fail the entire snapshot. No service is started.
pub fn local_snapshot() -> Result<LocalSnapshot> {
    let local = local_status()?;
    let policy = config::DaemonConfig::load()?;
    let mut network_error = None;
    let network = if local.joined {
        match network_status() {
            Ok(value) => Some(value),
            Err(error) => {
                network_error = Some(Data::error(&error));
                None
            }
        }
    } else {
        None
    };
    Ok(LocalSnapshot {
        local,
        network,
        allow_from: policy.allow_from,
        deny_from: policy.deny_from,
        network_error,
    })
}

fn network_status() -> Result<NetworkStatus> {
    let id = Identity::load()?;
    let signed = crate::network::current(&id)?;
    Ok(NetworkStatus {
        network_id: signed.roster.network_id.clone(),
        manager_id: signed.roster.manager_id.clone(),
        manager_name: signed.member(&signed.roster.manager_id)?.name.clone(),
        is_manager: id.device_id == signed.roster.manager_id,
        relay_addresses: signed.roster.relay_addresses,
    })
}
#[derive(Debug, Serialize)]
/// Local identity and service summary, without contacting remote devices.
pub struct LocalStatus {
    /// Whether a saved local identity exists.
    pub joined: bool,
    /// Immutable device ID, or absent when no identity is installed.
    pub device_id: Option<String>,
    /// Saved device name, or absent when no identity is installed.
    pub name: Option<String>,
    /// Current CLI/core package version.
    pub version: &'static str,
    /// Whether local identity and task storage were initialized.
    pub daemon_initialized: bool,
    /// Whether the daemon is registered with the platform supervisor.
    pub daemon_installed: bool,
    /// Whether a process holds the daemon instance lock.
    pub daemon_running: bool,
    /// Relay connection state; None when runtime metadata is unavailable.
    pub daemon_connected: Option<bool>,
    /// Whether all incoming remote operations are temporarily blocked.
    pub remote_access_paused: bool,
    /// Whether all current and future members are allowed, except individual denials.
    pub allow_all: bool,
    /// Whether this machine has local relay configuration.
    pub server_configured: bool,
    /// Whether the relay is registered with the platform supervisor.
    pub server_installed: bool,
    /// Whether a process holds the relay instance lock.
    pub server_running: bool,
}
#[derive(Debug, Serialize)]
/// Local status plus best-effort relay discovery; network failure is a partial result.
pub struct Status {
    /// Local state, always retained when only the network query fails.
    pub local: LocalStatus,
    /// Discovered members, absent when unjoined or discovery fails.
    pub devices: Option<Vec<Device>>,
    /// Coded discovery failure, independent of successfully read local state.
    pub server_error: Option<Data>,
}
/// Read local identity, configuration, locks and service registration. No relay IO
/// or file creation occurs. A running daemon can still be disconnected; malformed
/// configuration or identity fails the request.
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
/// Renew the local certificate if needed and query relay-connected members. Local
/// configuration failures fail the request; network failures populate server_error
/// with a machine code while retaining local status. No service is started.
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
