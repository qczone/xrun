//! Local authority passwords never enter argv, environment variables or diagnostics.
use super::{args::NetworkCommand, support::print};
use crate::{client, error::ErrorCode};
use anyhow::{Result, bail};
use std::io::{IsTerminal, Read};
use zeroize::Zeroizing;

fn password(from_stdin: bool, confirm: bool) -> Result<Zeroizing<String>> {
    if from_stdin {
        let mut input = Zeroizing::new(String::new());
        std::io::stdin().take(1026).read_to_string(&mut input)?;
        if input.ends_with('\n') {
            input.pop();
            if input.ends_with('\r') {
                input.pop();
            }
        }
        if input.contains(['\n', '\r']) || input.len() > 1024 {
            bail!(
                ErrorCode::InvalidConfig.error("provide one password line of at most 1024 bytes")
            );
        }
        return Ok(input);
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            ErrorCode::InteractiveRequired
                .error("use an interactive password prompt or --password-stdin")
        );
    }
    let password = Zeroizing::new(rpassword::prompt_password("Backup password: ")?);
    if confirm
        && password.as_str()
            != Zeroizing::new(rpassword::prompt_password("Confirm password: ")?).as_str()
    {
        bail!(ErrorCode::InvalidConfig.error("backup passwords differ"));
    }
    Ok(password)
}
pub(super) async fn manage(operation: NetworkCommand, json: bool) -> Result<()> {
    let info = match operation {
        NetworkCommand::Direct { address, off } => {
            if off || address.is_some() {
                client::save_direct_connection(address).await?;
            }
            let report = serde_json::json!({"lan_address":client::settings()?.lan_address,"interfaces":crate::lan::interfaces()?});
            print(json, &report, || {
                println!("{}", serde_json::to_string_pretty(&report).unwrap())
            });
            return Ok(());
        }
        NetworkCommand::Compression { mode } => {
            if let Some(mode) = mode {
                client::save_traffic_compression(mode).await?;
            }
            let mode = client::settings()?.traffic_compression;
            print(
                json,
                &serde_json::json!({"traffic_compression":mode}),
                || {
                    println!(
                        "compression: {}",
                        serde_json::to_value(mode).unwrap().as_str().unwrap()
                    )
                },
            );
            return Ok(());
        }
        NetworkCommand::Lease { minutes } => {
            if let Some(minutes) = minutes {
                client::save_membership_lease(minutes).await?;
            }
            let minutes = client::settings()?.membership_lease_minutes;
            print(
                json,
                &serde_json::json!({"membership_lease_minutes": minutes}),
                || println!("membership freshness: {minutes} minutes"),
            );
            return Ok(());
        }
        NetworkCommand::Backup {
            path,
            password_stdin,
        } => {
            let password = password(password_stdin, true)?;
            client::network::backup_authority(&path, password.as_bytes())?
        }
        NetworkCommand::Migrate {
            device,
            out,
            password_stdin,
        } => {
            let password = password(password_stdin, true)?;
            client::network::migrate_authority(&device, &out, password.as_bytes())?
        }
        NetworkCommand::Restore {
            path,
            password_stdin,
        } => {
            let password = password(password_stdin, false)?;
            let info = client::network::restore_authority(&path, password.as_bytes()).await?;
            client::services::initialize_daemon()?;
            info
        }
    };
    print(json, &info, || {
        println!(
            "authority generation {} · {}",
            info.authority_generation, info.network_id
        )
    });
    Ok(())
}
