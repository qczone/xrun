//! Local network, access and daemon management commands.
use crate::error::ErrorCode;
use crate::{
    config::{self, ServerConfig},
    daemon, service,
    store::SubmissionStore,
};
use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, Write};

use super::args::{DaemonCommand, Local, LocalCli, PermissionArgs};
use super::relay::run_relay;
use super::support::*;

pub(super) async fn run(cli: LocalCli) -> Result<i32> {
    let json = cli.json;
    match cli.command {
        Local::Network { operation } => super::network::manage(operation, json).await?,
        Local::Connect {
            link,
            code,
            name,
            allow_manager,
            no_daemon,
        } => {
            let link = match code {
                Some(code) => crate::client::code_link(&link, &code)?,
                None => link,
            };
            let joining =
                crate::client::connection_kind(&link)? == crate::client::ConnectionKind::Join;
            let joined = crate::client::connect(&link, name, allow_manager).await?;
            if joining && !allow_manager && !json {
                consent_to_manager().await?;
            }
            daemon::init()?;
            if !no_daemon {
                service::install("daemon").await?;
            }
            print(json, &joined, || {
                println!("{} ({})", joined.name, joined.device_id)
            });
        }
        Local::Up {
            relay,
            name,
            no_daemon,
            allow,
        } => up(&relay, name, no_daemon, allow, json).await?,
        Local::Relay { operation } => super::relay::manage(operation, json).await?,
        Local::Join {
            link,
            name,
            no_daemon,
            allow_manager,
        } => {
            // join remains the explicit invitation-only entry point.
            if crate::client::connection_kind(&link)? != crate::client::ConnectionKind::Join {
                bail!(ErrorCode::InvalidLink.error("join requires a member invitation"))
            }
            let id = if allow_manager {
                crate::client::connect(&link, name, true).await?
            } else {
                crate::client::join(&link, name).await?
            };
            if !allow_manager && !json {
                consent_to_manager().await?;
            }
            daemon::init()?;
            if !no_daemon {
                service::install("daemon").await?
            }
            print(
                json,
                &serde_json::json!({"device_id":id.device_id,"name":id.name}),
                || println!("{} ({})", id.name, id.device_id),
            );
        }
        Local::Invite { allow, link } => {
            let invitation = if link {
                crate::client::invite_link(allow).await?
            } else {
                crate::client::invite(allow).await?
            };
            let value = serde_json::to_value(&invitation)?;
            if value["allow"].as_bool() != Some(allow) {
                bail!(
                    ErrorCode::VersionMismatch
                        .error("invitation policy differs; upgrade all components")
                )
            }
            print(json, &value, || {
                if let (Some(address), Some(code)) = (&invitation.address, &invitation.code) {
                    println!(
                        "relay: {address}\naccess code: {code}\nxrun connect {address} {code}"
                    );
                } else {
                    println!("{}", invitation.link);
                }
            });
            if !json {
                invitation_notice(allow);
            }
        }
        Local::AllowFrom(args) => permission(args, true, json).await?,
        Local::DenyFrom(args) => permission(args, false, json).await?,
        Local::Revoke { device } => {
            let v = serde_json::to_value(crate::client::revoke(&device).await?)?;
            print(json, &v, || {
                println!(
                    "revoked {} (roster {})",
                    v["device_id"], v["roster_version"]
                );
                if let Some(devices) = v["undelivered"].as_array()
                    && !devices.is_empty()
                {
                    eprintln!(
                        "[xrun] not confirmed by: {}",
                        devices
                            .iter()
                            .filter_map(|d| d.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                }
                if let Some(error) = v["sync_error"].as_str() {
                    eprintln!(
                        "[xrun] signed revocation saved locally; peer synchronization failed: {error}"
                    );
                }
            });
        }
        Local::Status => {
            return status(json).await;
        }
        Local::Traffic {
            period,
            limit,
            offset,
        } => {
            let report = crate::client::traffic(crate::protocol::TrafficQuery {
                period,
                limit,
                offset,
            })
            .await?;
            print(json, &report, || {
                println!(
                    "traffic: {} ({period}, UTC)",
                    report.device_id.as_deref().unwrap_or(&report.network_id)
                );
                println!(
                    "{}: {} bytes",
                    if report.device_id.is_some() {
                        "sent"
                    } else {
                        "ingress"
                    },
                    report.totals.ingress_bytes
                );
                println!(
                    "{}: {} bytes",
                    if report.device_id.is_some() {
                        "received"
                    } else {
                        "egress"
                    },
                    report.totals.egress_bytes
                );
                for device in &report.devices {
                    println!(
                        "{}\tsent={}\treceived={}",
                        device.device_id, device.sent_bytes, device.received_bytes
                    );
                }
                if let Some(offset) = report.next_offset {
                    println!("next page: --offset {offset}");
                }
                if !report.complete {
                    eprintln!("[xrun] traffic counters are incomplete after a recording error");
                }
            });
        }
        Local::Recent => {
            let submissions =
                SubmissionStore::open(&config::device_dir()?.join("submissions.sqlite"))?
                    .recent()?;
            print(json, &submissions, || {
                for s in &submissions {
                    println!(
                        "{}\t{}\t{}\t{}",
                        s.request_id,
                        s.job_id.as_deref().unwrap_or("-"),
                        s.target_device_id,
                        s.status
                    );
                }
            })
        }
        Local::Down { purge } => {
            service::uninstall("daemon").await?;
            crate::client::services::finish_daemon_restart()?;
            if cfg!(target_os = "linux") {
                service::uninstall("server").await?
            }
            if purge {
                if !std::io::stdin().is_terminal() {
                    bail!(
                        ErrorCode::InteractiveRequired
                            .error("--purge requires terminal confirmation")
                    )
                };
                eprint!("Delete all local xrun identity, CA, jobs and logs? Type purge: ");
                std::io::stderr().flush()?;
                let mut value = String::new();
                std::io::stdin().read_line(&mut value)?;
                if value.trim() != "purge" {
                    return Ok(0);
                }
                let _lock = daemon::instance_lock()?;
                let _server_lock = ServerConfig::load()
                    .ok()
                    .map(|cfg| crate::server::instance_lock(&cfg.data_dir))
                    .transpose()?;
                std::fs::remove_dir_all(config::device_dir()?)?;
            }
        }
        Local::Server => run_relay().await?,
        Local::Daemon { operation } => match operation {
            None => daemon::run().await?,
            Some(DaemonCommand::Install) => {
                daemon::init()?;
                service::install("daemon").await?
            }
            Some(DaemonCommand::Uninstall) => {
                service::uninstall("daemon").await?;
                crate::client::services::finish_daemon_restart()?;
            }
            Some(DaemonCommand::Start) => service::start("daemon").await?,
            Some(DaemonCommand::Stop) => crate::client::services::stop_daemon().await?,
            Some(DaemonCommand::Reset) => daemon::reset()?,
            Some(DaemonCommand::Pause) => {
                crate::client::pause_access(true).await?;
            }
            Some(DaemonCommand::Resume) => {
                crate::client::pause_access(false).await?;
            }
        },
        Local::Doc { topic, list, lang } => super::docs::print(topic, list, lang),
        Local::Help { command } => return super::help::run(&command),
    }
    Ok(0)
}
async fn status(json: bool) -> Result<i32> {
    let status = crate::client::status().await?;
    print(json, &status, || {
        if let (Some(name), Some(id)) = (&status.local.name, &status.local.device_id) {
            println!("local: {name} ({id})");
        } else {
            println!("local: not joined");
        }
        println!(
            "daemon: {}",
            if status.local.daemon_running {
                "running"
            } else {
                "stopped"
            }
        );
        println!(
            "remote access: {}",
            if status.local.remote_access_paused {
                "paused"
            } else {
                "enabled"
            }
        );
        println!(
            "trust: {}",
            if status.local.allow_all {
                "all current and future members (individual denials apply)"
            } else {
                "individually allowed devices"
            }
        );
        if status.local.server_configured {
            println!(
                "server: {}",
                if status.local.server_running {
                    "running"
                } else {
                    "stopped"
                }
            );
        }
        if let Some(devices) = &status.devices {
            for d in devices {
                println!(
                    "{}\t{}\t{}",
                    d.name,
                    d.device_id,
                    if d.revoked {
                        "revoked"
                    } else if d.online {
                        "relay-connected"
                    } else {
                        "disconnected"
                    }
                );
            }
        }
        if let Some(error) = &status.server_error {
            eprintln!("[xrun] device list unavailable: {error:?}");
        }
    });
    Ok(if status.server_error.is_some() {
        125
    } else {
        0
    })
}
async fn permission(args: PermissionArgs, allow: bool, json: bool) -> Result<()> {
    if args.all {
        crate::client::set_all_permissions(allow).await?;
        print(
            json,
            &serde_json::json!({"all":true,"allowed":allow}),
            || {
                println!(
                    "all-member access {}; individual permissions retained",
                    if allow {
                        "enabled (includes future members)"
                    } else {
                        "disabled"
                    }
                );
            },
        );
        return Ok(());
    }
    let value = args
        .device
        .as_deref()
        .context(ErrorCode::InvalidRequest.error("device required"))?;
    let result = crate::client::set_permission(value, allow).await?;
    print(json, &result, || {
        println!(
            "{} {}",
            if allow { "allowed" } else { "denied" },
            result.source_device_id
        )
    });
    Ok(())
}
async fn consent_to_manager() -> Result<()> {
    use std::io::{IsTerminal, Write};
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(());
    }
    let id = config::Identity::load()?;
    let roster = crate::network::current(&id)?;
    let manager = &roster.roster.manager_id;
    let name = &roster.member(manager)?.name;
    eprint!("[xrun] allow {name} to access this device? [y/N] ");
    std::io::stderr().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        crate::client::set_permission(manager, true).await?;
    }
    Ok(())
}
async fn up(
    relay: &str,
    name: Option<String>,
    no_daemon: bool,
    allow: bool,
    json: bool,
) -> Result<()> {
    let id = crate::client::create_network(relay, name).await?;
    daemon::init()?;
    if !no_daemon {
        service::install("daemon").await?;
    }
    let invitation = crate::client::invite_link(allow).await?;
    print(
        json,
        &serde_json::json!({
            "device_id":id.device_id, "network_id":id.network_id, "link":invitation.link,
            "addresses":id.relay_addresses, "allow":allow, "expires_in":invitation.expires_in,
        }),
        || {
            println!("manager: {} ({})", id.name, id.device_id);
            println!("xrun join '{}'", invitation.link);
        },
    );
    if !json {
        invitation_notice(allow);
    }
    Ok(())
}
fn invitation_notice(allow: bool) {
    eprintln!("[xrun] invitation is a secret, valid once for 10 minutes");
    if allow {
        eprintln!(
            "[xrun] --allow requests manager access to the joining device; its owner must confirm"
        );
    }
}
