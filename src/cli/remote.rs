//! Target selection, submission identity and remote command dispatch.
use crate::error::ErrorCode;
use crate::{config, crypto, net, protocol::*, store::SubmissionStore};
use anyhow::{Context, Result, bail};

use super::args::{DeviceCli, Remote};
use super::support::*;
use super::{execution, files, forward, jobs};

pub(super) async fn run(cli: DeviceCli) -> Result<i32> {
    let json = cli.json;
    if matches!(&cli.command, Remote::Start(e) if e.interactive)
        || (json && matches!(&cli.command, Remote::Run(e) | Remote::Start(e) if e.interactive))
    {
        eprintln!("[xrun] -i cannot be combined with start or --json");
        return Ok(2);
    }
    if let Remote::Jobs {
        id: Some(_),
        running,
        request_id,
        ..
    } = &cli.command
        && (*running || request_id.is_some())
    {
        eprintln!("[xrun] jobs <ID> cannot be combined with list filters");
        return Ok(2);
    }
    if matches!(&cli.command,Remote::Pull{local:Some(local),..}if local=="-"&&json) {
        eprintln!("[xrun] pull to stdout cannot be combined with --json");
        return Ok(2);
    }
    let id = identity().await?;
    let explicit = match &cli.command {
        Remote::Run(e) | Remote::Start(e) => e.request_id.as_deref(),
        _ => None,
    };
    let submissions = if matches!(&cli.command, Remote::Run(e) | Remote::Start(e) if !e.interactive)
    {
        Some(SubmissionStore::open(
            &config::device_dir()?.join("submissions.sqlite"),
        )?)
    } else {
        None
    };
    let prior = explicit
        .map(|r| submissions.as_ref().context("submission store")?.get(r))
        .transpose()?
        .flatten();
    if prior
        .as_ref()
        .is_some_and(|p| cli.device != p.target_device_id && cli.device != p.target_name)
    {
        diagnostic(
            json,
            &anyhow::anyhow!(
                ErrorCode::DeviceMismatch.error("request-id belongs to another target device")
            ),
        );
        return Ok(2);
    }
    if prior.as_ref().is_some_and(|s| {
        s.source_device_id != id.device_id
            || crypto::ca_spki_pin(&id.ca_pem).ok().as_deref() != Some(&s.ca_pin)
    }) {
        bail!(
            ErrorCode::IdentityMismatch
                .error("submission belongs to another identity or deployment")
        )
    }
    let selected = prior
        .as_ref()
        .map(|s| s.target_device_id.as_str())
        .unwrap_or(&cli.device);
    // The operation session authenticates the target and exchanges the latest
    // signed roster. Only info needs a separate live state query.
    let roster = crate::network::current(&id)?;
    let target = roster.member(selected)?;
    if target.revoked {
        bail!(ErrorCode::DeviceRevoked.error("target has been revoked"))
    }
    let target_name = target.name.clone();
    let target = &target.device_id;
    match cli.command {
        Remote::Run(e) if e.interactive => execution::stream(&id, target, e).await,
        Remote::Run(e) => {
            Box::pin(execution::run(
                &id,
                (target, &target_name),
                e,
                false,
                json,
                submissions.as_ref().context("submission store")?,
                prior,
            ))
            .await
        }
        Remote::Start(e) => {
            Box::pin(execution::run(
                &id,
                (target, &target_name),
                e,
                true,
                json,
                submissions.as_ref().context("submission store")?,
                prior,
            ))
            .await
        }
        Remote::Info => {
            let target_metadata: Device = net::http(
                &id,
                reqwest::Method::GET,
                &format!("/devices/{target}"),
                None,
            )
            .await?;
            print(json, &target_metadata, || {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&target_metadata).unwrap()
                )
            });
            Ok(0)
        }
        Remote::Forward { ports } => forward::run(id, target, ports, json).await,
        command @ (Remote::Jobs { .. }
        | Remote::Wait { .. }
        | Remote::Logs { .. }
        | Remote::Kill { .. }) => jobs::run(&id, target, &target_name, command, json).await,
        command @ (Remote::Push { .. } | Remote::Pull { .. } | Remote::Screenshot { .. }) => {
            files::run(&id, target, command, json).await
        }
    }
}
