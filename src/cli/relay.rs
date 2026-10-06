//! Linux relay deployment and address detection.
use crate::error::ErrorCode;
use crate::{
    config::{self, ServerConfig},
    daemon, service,
};
use anyhow::{Result, bail};
use std::time::Duration;

use super::support::*;

async fn detect_addresses(port: u16, no_detect: bool) -> Result<Vec<String>> {
    let mut addresses = vec![];
    #[cfg(unix)]
    unsafe {
        let mut first = std::ptr::null_mut();
        if libc::getifaddrs(&mut first) == 0 {
            let mut current = first;
            while !current.is_null() {
                let item = &*current;
                if !item.ifa_addr.is_null() && (*item.ifa_addr).sa_family as i32 == libc::AF_INET {
                    let addr = &*(item.ifa_addr as *const libc::sockaddr_in);
                    let ip = std::net::Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes());
                    if !ip.is_loopback() && !ip.is_link_local() && !ip.is_unspecified() {
                        addresses.push(format!("{ip}:{port}"));
                    }
                }
                current = item.ifa_next;
            }
            libc::freeifaddrs(first);
        }
    }
    if !no_detect {
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            let client = reqwest::Client::builder()
                .no_proxy()
                .local_address(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
                .build()?;
            let value = client
                .get("https://api.ipify.org")
                .send()
                .await?
                .text()
                .await?;
            Ok::<_, anyhow::Error>(value.trim().parse::<std::net::Ipv4Addr>()?)
        })
        .await;
        if let Ok(Ok(ip)) = result {
            addresses.push(format!("{ip}:{port}"));
        }
    }
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        bail!(ErrorCode::NoAddress.error("provide --addr <host>:<port>"))
    }
    Ok(addresses)
}
pub(super) async fn run_relay() -> Result<()> {
    require_linux()?;
    let mut cfg = ServerConfig::load()?;
    if !cfg.manual {
        cfg.addresses = detect_addresses(cfg.port, cfg.no_detect).await?;
        cfg.save()?;
    }
    tokio::select! {result=crate::relay::run(cfg)=>result,_=daemon::shutdown_signal()=>Ok(())}
}
pub(super) async fn relay_install(
    port: Option<u16>,
    addresses: Vec<String>,
    no_detect: bool,
    json: bool,
) -> Result<()> {
    require_linux()?;
    let dir = config::device_dir()?;
    let before = ServerConfig::load().ok();
    let mut cfg = before.clone().unwrap_or(ServerConfig {
        port: port.unwrap_or(9528),
        addresses: vec![],
        manual: false,
        no_detect,
        data_dir: dir.join("server"),
    });
    if let Some(port) = port {
        cfg.port = port;
    }
    if cfg.port == 0 {
        bail!(ErrorCode::InvalidPort.error("port must be 1..65535"))
    }
    if !addresses.is_empty() {
        for address in &addresses {
            crate::client::validate_address(address)?;
        }
        cfg.addresses = addresses;
        cfg.manual = true;
    } else if !cfg.manual {
        cfg.no_detect |= no_detect;
        cfg.addresses = detect_addresses(cfg.port, cfg.no_detect).await?;
    }
    cfg.save()?;
    let link = crate::relay::deployment_link(&cfg)?;
    service::install("server").await?;
    if before.is_some_and(|old| old.port != cfg.port || old.addresses != cfg.addresses) {
        service::restart("server").await?;
    }
    print(
        json,
        &serde_json::json!({"link":link,"addresses":crate::relay::addresses(&cfg)?}),
        || {
            println!("relay: {}", cfg.addresses.join(", "));
            println!("xrun up --relay '{link}'");
        },
    );
    eprintln!(
        "[xrun] keep the relay link private; allow inbound TCP {}",
        cfg.port
    );
    Ok(())
}
