//! Provider-aware relay commands and local Linux deployment.
use crate::error::ErrorCode;
use crate::{
    config::{self, ServerConfig},
    daemon, service,
};
use anyhow::{Result, bail};
use std::time::Duration;

use super::args::{RelayCommand, RelayProvider};
use super::support::*;
mod progress;

fn require_provider(actual: RelayProvider, expected: RelayProvider, command: &str) -> Result<()> {
    if actual != expected {
        let provider = match expected {
            RelayProvider::Cloudflare => "cloudflare",
            RelayProvider::Local => "local",
        };
        bail!(ErrorCode::InvalidRequest.error(format!(
            "relay {command} supports only --provider {provider}"
        )))
    }
    Ok(())
}

pub(super) async fn manage(operation: RelayCommand, json: bool) -> Result<()> {
    let reporter = progress::reporter(&operation, json)?;
    let deployment_result = json
        && matches!(
            &operation,
            RelayCommand::Deploy {
                provider: RelayProvider::Cloudflare,
                dry_run: false,
                ..
            }
        );
    let cancellable = matches!(
        &operation,
        RelayCommand::Deploy {
            provider: RelayProvider::Cloudflare,
            dry_run: false,
            ..
        } | RelayCommand::Login {
            provider: RelayProvider::Cloudflare,
            ..
        } | RelayCommand::Logout {
            provider: RelayProvider::Cloudflare
        } | RelayCommand::Remove {
            provider: RelayProvider::Cloudflare,
            ..
        } | RelayCommand::Check {
            provider: RelayProvider::Cloudflare,
            ..
        }
    );
    reporter
        .scope(async {
            let result = if cancellable {
                tokio::select! {
                    result = manage_operation(operation, json) => result,
                    Ok(()) = tokio::signal::ctrl_c() => {
                        let cancelled = || ErrorCode::OAuthCancelled.error(
                            "relay operation cancelled; saved deployments can be resumed with the same --name");
                        let cleanup = crate::cloudflare::auth::finish_temporary::<()>(Err(cancelled().into()));
                        tokio::select! {
                            result = cleanup => result,
                            Ok(()) = tokio::signal::ctrl_c() => Err(cancelled().into()),
                        }
                    }
                }
            } else { manage_operation(operation, json).await };
            if deployment_result {
                progress::deployment_result(result.as_ref().err())?;
            }
            reporter.report(
                crate::deployment::DeploymentStage::Complete,
                if result.is_ok() {
                    crate::deployment::ProgressStatus::Completed
                } else if result
                    .as_ref()
                    .err()
                    .is_some_and(|error| crate::error::is(error, ErrorCode::OAuthCancelled))
                {
                    crate::deployment::ProgressStatus::Cancelled
                } else {
                    crate::deployment::ProgressStatus::Failed
                },
                0,
                crate::deployment::ProgressDetails {
                    error: result.as_ref().err().map(crate::protocol::Data::error),
                    ..Default::default()
                },
            );
            result
        })
        .await
}

async fn manage_operation(operation: RelayCommand, json: bool) -> Result<()> {
    match operation {
        RelayCommand::Deploy {
            provider,
            name,
            account,
            region,
            no_browser,
            dry_run,
            port,
            addr,
            no_detect,
        } => match provider {
            RelayProvider::Cloudflare => {
                if port.is_some() || !addr.is_empty() || no_detect {
                    bail!(
                        ErrorCode::InvalidRequest
                            .error("--port, --addr and --no-detect require --provider local")
                    )
                }
                let name = name.as_deref().unwrap_or("xrun-relay");
                if dry_run {
                    let mut bundle = crate::cloudflare::check(name)?;
                    bundle["region"] = serde_json::json!(region.unwrap_or_default());
                    print(json, &bundle, || {
                        println!(
                            "Cloudflare relay {} is bundled (OAuth configured: {})",
                            crate::protocol::VERSION,
                            bundle["oauth_configured"]
                        )
                    });
                } else {
                    let relay =
                        crate::cloudflare::deploy(name, account, no_browser, region).await?;
                    if !json {
                        println!("relay: {}", relay.link);
                        println!("xrun up --relay '{}'", relay.link);
                    }
                }
            }
            RelayProvider::Local => {
                if name.is_some() || account.is_some() || region.is_some() || no_browser || dry_run
                {
                    bail!(ErrorCode::InvalidRequest.error("--name, --account, --region, --no-browser and --dry-run require --provider cloudflare"))
                }
                deploy_local(port, addr, no_detect, json).await?;
            }
        },
        RelayCommand::Login {
            provider,
            no_browser,
            remember,
        } => {
            require_provider(provider, RelayProvider::Cloudflare, "login")?;
            crate::cloudflare::login(no_browser, remember).await?;
            print(
                json,
                &serde_json::json!({"provider":"cloudflare", "remembered":remember}),
                || {
                    println!(
                        "{}",
                        if remember {
                            "Cloudflare login saved in system credential store"
                        } else {
                            "Temporary Cloudflare authorization verified and revoked"
                        }
                    )
                },
            );
        }
        RelayCommand::Logout { provider } => {
            require_provider(provider, RelayProvider::Cloudflare, "logout")?;
            crate::cloudflare::logout().await?;
            print(
                json,
                &serde_json::json!({"provider":"cloudflare", "authenticated":false}),
                || println!("Cloudflare logout complete"),
            );
        }
        RelayCommand::Check { provider, name } => {
            let link = match provider {
                RelayProvider::Cloudflare => {
                    crate::cloudflare::link(name.as_deref().unwrap_or("xrun-relay"))?.link
                }
                RelayProvider::Local => {
                    if name.is_some() {
                        bail!(
                            ErrorCode::InvalidRequest
                                .error("--name requires --provider cloudflare")
                        );
                    }
                    require_linux()?;
                    crate::relay::deployment_link(&ServerConfig::load()?)?
                }
            };
            crate::client::relay::check_relay(&link).await?;
            let provider = match provider {
                RelayProvider::Cloudflare => "cloudflare",
                RelayProvider::Local => "local",
            };
            print(
                json,
                &serde_json::json!({"provider":provider,"ready":true,"link":link}),
                || println!("relay ready: {link}"),
            );
        }
        RelayCommand::Run { provider } => {
            require_provider(provider, RelayProvider::Local, "run")?;
            run_relay().await?;
        }
        RelayCommand::Link { provider, name } => match provider {
            RelayProvider::Cloudflare => {
                let relay = crate::cloudflare::link(name.as_deref().unwrap_or("xrun-relay"))?;
                print(json, &relay, || println!("{}", relay.link));
            }
            RelayProvider::Local => {
                if name.is_some() {
                    bail!(ErrorCode::InvalidRequest.error("--name requires --provider cloudflare"))
                }
                require_linux()?;
                let link = crate::relay::deployment_link(&ServerConfig::load()?)?;
                print(
                    json,
                    &serde_json::json!({"provider":"local", "link":link}),
                    || println!("{link}"),
                );
            }
        },
        RelayCommand::Remove {
            provider,
            name,
            purge,
            no_browser,
        } => match provider {
            RelayProvider::Cloudflare => {
                if !purge {
                    bail!(ErrorCode::InvalidRequest.error("Cloudflare removal permanently deletes Durable Objects and traffic data; specify --purge"))
                }
                let removed =
                    crate::cloudflare::remove(name.as_deref().unwrap_or("xrun-relay"), no_browser)
                        .await?;
                print(json, &removed, || {
                    println!("Cloudflare relay {} removed", removed.name)
                });
            }
            RelayProvider::Local => {
                if name.is_some() || purge || no_browser {
                    bail!(
                        ErrorCode::InvalidRequest.error(
                            "--name, --purge and --no-browser require --provider cloudflare"
                        )
                    )
                }
                require_linux()?;
                service::uninstall("server").await?;
                print(
                    json,
                    &serde_json::json!({"provider":"local", "removed":true, "data_retained":true}),
                    || println!("Local relay service removed; data retained"),
                );
            }
        },
    }
    Ok(())
}

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
async fn deploy_local(
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
    crate::deployment::step(crate::deployment::DeploymentStage::ServiceStart, async {
        service::install("server").await?;
        if before.is_some_and(|old| old.port != cfg.port || old.addresses != cfg.addresses) {
            service::restart("server").await?;
        }
        Ok(())
    })
    .await?;
    crate::client::relay::check_relay(&link).await?;
    print(
        json,
        &serde_json::json!({"provider":"local","link":link,"addresses":crate::relay::addresses(&cfg)?}),
        || {
            println!("relay: {}", cfg.addresses.join(", "));
            println!("xrun up --relay '{link}'");
        },
    );
    if !json {
        eprintln!("[xrun] allow inbound TCP {}", cfg.port);
    }
    Ok(())
}
