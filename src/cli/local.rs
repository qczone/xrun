//! Local network, access and daemon management commands.
use crate::error::ErrorCode;
use crate::{
    config::{self, Identity, ServerConfig},
    daemon, net, service,
    store::SubmissionStore,
};
use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, Write};

use super::args::{DaemonCommand, Local, LocalCli, PermissionArgs, RelayCommand};
use super::relay::{relay_install, run_relay};
use super::support::*;

pub(super) async fn run(cli: LocalCli) -> Result<i32> {
    let json = cli.json;
    match cli.command {
        Local::Up {
            relay,
            name,
            no_daemon,
            allow,
        } => up(&relay, name, no_daemon, allow, json).await?,
        Local::Relay { operation } => match operation {
            RelayCommand::Install {
                port,
                addr,
                no_detect,
            } => relay_install(port, addr, no_detect, json).await?,
            RelayCommand::Run => run_relay().await?,
            RelayCommand::Invite => {
                require_linux()?;
                let link = crate::relay::deployment_link(&ServerConfig::load()?)?;
                print(json, &serde_json::json!({"link":link}), || {
                    println!("{link}")
                });
            }
            RelayCommand::Uninstall => {
                require_linux()?;
                service::uninstall("server").await?;
            }
        },
        Local::Join {
            link,
            name,
            no_daemon,
        } => {
            crate::client::join(&link, name).await?;
            let id = Identity::load()?;
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
        Local::Invite { allow } => {
            let id = identity().await?;
            let value: serde_json::Value = net::http(
                &id,
                reqwest::Method::POST,
                "/invites",
                Some(serde_json::json!({"allow":allow})),
            )
            .await?;
            if value["allow"].as_bool() != Some(allow) {
                bail!(
                    ErrorCode::VersionMismatch
                        .error("invitation policy differs; upgrade all components")
                )
            }
            print(json, &value, || {
                println!("{}", value["link"].as_str().unwrap_or_default())
            });
            invitation_notice(allow);
        }
        Local::AllowFrom(args) => permission(args, true, json).await?,
        Local::DenyFrom(args) => permission(args, false, json).await?,
        Local::Revoke { device } => {
            let id = identity().await?;
            let v: serde_json::Value = net::http(
                &id,
                reqwest::Method::POST,
                "/admin/revoke",
                Some(serde_json::json!({"device":device})),
            )
            .await?;
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
            Some(DaemonCommand::Uninstall) => service::uninstall("daemon").await?,
            Some(DaemonCommand::Start) => service::start("daemon").await?,
            Some(DaemonCommand::Stop) => service::stop_daemon().await?,
            Some(DaemonCommand::Reset) => daemon::reset()?,
            Some(DaemonCommand::Pause) => {
                config::pause_remote_access(true)?;
                crate::control::refresh_access().await?;
            }
            Some(DaemonCommand::Resume) => {
                config::pause_remote_access(false)?;
                crate::control::refresh_access().await?;
            }
        },
        Local::Doc { topic, list } => super::docs::print(topic, list),
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
        config::update_all_permissions(allow)?;
        crate::control::refresh_access().await?;
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
async fn up(
    relay: &str,
    name: Option<String>,
    no_daemon: bool,
    allow: bool,
    json: bool,
) -> Result<()> {
    let id = crate::network::create(relay, name).await?;
    daemon::init()?;
    if !no_daemon {
        service::install("daemon").await?;
    }
    let invitation = crate::network::invite(&id, allow).await?;
    print(
        json,
        &serde_json::json!({"device_id":id.device_id,"network_id":crate::network::authority(&id)?.network_id,"link":invitation["link"],"addresses":id.addresses,"allow":allow}),
        || {
            println!("manager: {} ({})", id.name, id.device_id);
            println!(
                "xrun join '{}'",
                invitation["link"].as_str().unwrap_or_default()
            );
        },
    );
    invitation_notice(allow);
    Ok(())
}
fn invitation_notice(allow: bool) {
    eprintln!("[xrun] invitation is a secret, valid once for 10 minutes");
    if allow {
        eprintln!(
            "[xrun] --allow grants mutual command execution as the device's user; share only with a trusted device"
        );
    }
}
