//! Explicit private-interface discovery and authenticated, bounded direct connection attempts.
use crate::{
    config::{self, Identity},
    crypto,
    error::ErrorCode,
    net::{self, Ws},
    protocol::*,
    secure,
};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::protocol::{Role, WebSocketConfig},
};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A selected private adapter, never a wildcard or public listener.
pub struct Interface {
    /// OS adapter name.
    pub name: String,
    /// Assigned private IPv4 address.
    pub address: Ipv4Addr,
    /// OS IPv4 mask.
    pub netmask: Ipv4Addr,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Candidate disclosed only inside authenticated end-to-end metadata exchange.
pub struct Candidate {
    /// Bound private socket.
    pub address: SocketAddrV4,
    /// Selected adapter prefix.
    pub netmask: Ipv4Addr,
}
static LOCAL: OnceLock<Mutex<Option<Candidate>>> = OnceLock::new();
pub(crate) fn local() -> Option<Candidate> {
    LOCAL.get_or_init(Mutex::default).lock().unwrap().clone()
}
pub(crate) fn advertise(value: Option<Candidate>) {
    *LOCAL.get_or_init(Mutex::default).lock().unwrap() = value;
}
/// Enumerate assigned private IPv4 interfaces. Loopback is local-only and useful for isolated devices.
pub fn interfaces() -> Result<Vec<Interface>> {
    Ok(if_addrs::get_if_addrs()?
        .into_iter()
        .filter_map(|interface| match interface.addr {
            if_addrs::IfAddr::V4(addr) if addr.ip.is_private() || addr.ip.is_loopback() => {
                Some(Interface {
                    name: interface.name,
                    address: addr.ip,
                    netmask: addr.netmask,
                })
            }
            _ => None,
        })
        .collect())
}
pub(crate) fn selected() -> Result<Option<Interface>> {
    let Some(address) = config::DaemonConfig::load()?.lan_address else {
        return Ok(None);
    };
    interfaces()?
        .into_iter()
        .find(|interface| interface.address == address)
        .map(Some)
        .context(ErrorCode::InvalidConfig.error("selected private interface is unavailable"))
}
pub(crate) fn reachable(source: &Interface, target: &Candidate) -> bool {
    let ip = *target.address.ip();
    if target.address.port() == 0
        || ip.is_unspecified()
        || ip.is_multicast()
        || !(ip.is_private() || ip.is_loopback())
    {
        return false;
    }
    if source.address.is_loopback() || ip.is_loopback() {
        return source.address.is_loopback() && ip.is_loopback();
    }
    let mask = u32::from(source.netmask);
    let other = u32::from(target.netmask);
    mask != 0
        && other != 0
        && u32::from(source.address) & mask == u32::from(ip) & mask
        && u32::from(source.address) & other == u32::from(ip) & other
}
fn path(id: &Identity) -> Result<std::path::PathBuf> {
    Ok(config::network_dir()?.join(format!(
        "lan-peers-{}.json",
        crate::crypto::ca_spki_pin(&id.ca_pem)?
    )))
}
pub(crate) fn remember(id: &Identity, target: &str, candidate: Option<Candidate>) -> Result<()> {
    let path = path(id)?;
    let mut values: std::collections::BTreeMap<String, Candidate> = if path.exists() {
        serde_json::from_slice(&std::fs::read(&path)?)?
    } else {
        Default::default()
    };
    if let Some(candidate) = candidate {
        values.insert(target.into(), candidate);
    } else {
        values.remove(target);
    }
    values.retain(|target, _| {
        crate::network::current(id)
            .is_ok_and(|roster| roster.member(target).is_ok_and(|member| !member.revoked))
    });
    config::atomic_private_write(&path, &serde_json::to_vec(&values)?)
}
pub(crate) fn candidate(id: &Identity, target: &str) -> Option<Candidate> {
    let values: std::collections::BTreeMap<String, Candidate> =
        serde_json::from_slice(&std::fs::read(path(id).ok()?).ok()?).ok()?;
    values.get(target).cloned()
}
pub(crate) async fn connect(
    id: &Identity,
    target: &str,
    candidate: &Candidate,
) -> Result<(Ws, i64)> {
    let source =
        selected()?.context(ErrorCode::InvalidConfig.error("direct connections disabled"))?;
    if !reachable(&source, candidate) {
        bail!(ErrorCode::InvalidRequest.error("candidate is outside the selected private prefix"));
    }
    let socket = tokio::net::TcpSocket::new_v4()?;
    socket.bind(std::net::SocketAddr::V4(SocketAddrV4::new(
        source.address,
        0,
    )))?;
    let tcp = tokio::time::timeout(
        Duration::from_millis(800),
        socket.connect(std::net::SocketAddr::V4(candidate.address)),
    )
    .await??;
    tcp.set_nodelay(true)?;
    let outer = WebSocketStream::from_raw_socket(
        net::SocketIo::new(Box::new(tcp)),
        Role::Client,
        Some(
            WebSocketConfig::default()
                .max_message_size(Some(MAX_MESSAGE))
                .max_frame_size(Some(MAX_MESSAGE)),
        ),
    )
    .await;
    let (mut ws, cert) =
        tokio::time::timeout(Duration::from_secs(3), secure::client(outer, id, target)).await??;
    let network = &id.network.as_ref().context("joined network")?.network_id;
    tokio::time::timeout(
        Duration::from_secs(3),
        secure::exchange_client(
            &mut ws,
            &crate::network::cache()?,
            network,
            &cert,
            target,
            &secure::Purpose::Channel,
        ),
    )
    .await??;
    let (_, cert) = x509_parser::parse_x509_certificate(&cert)
        .map_err(|_| ErrorCode::InvalidCertificate.error("malformed target certificate"))?;
    Ok((
        ws,
        cert.validity()
            .not_after
            .timestamp()
            .min(crypto::certificate_expiry(&id.cert_pem)?),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn candidates_are_limited_to_the_selected_private_prefix() {
        let source = Interface {
            name: "owned".into(),
            address: "192.168.5.10".parse().unwrap(),
            netmask: "255.255.255.0".parse().unwrap(),
        };
        let target = |ip: &str| Candidate {
            address: SocketAddrV4::new(ip.parse().unwrap(), 9528),
            netmask: source.netmask,
        };
        assert!(reachable(&source, &target("192.168.5.20")));
        for address in [
            "192.168.6.20",
            "43.153.36.146",
            "0.0.0.0",
            "127.0.0.1",
            "224.0.0.1",
        ] {
            assert!(!reachable(&source, &target(address)));
        }
    }
}
