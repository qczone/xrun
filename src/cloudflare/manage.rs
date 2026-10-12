use super::{
    api::Api,
    auth,
    deploy::DeployedRelay,
    state::{self, Deployment, DeploymentStatus},
};
use crate::{config, error::ErrorCode};
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::Serialize;
use serde_json::json;
use std::path::Path;

const CLEANUP: &str =
    "export default {fetch(){return new Response('Relay removed',{status:410})}};";

#[derive(Serialize)]
pub(crate) struct RemovedRelay {
    pub provider: &'static str,
    pub name: String,
    pub removed: bool,
    pub data_retained: bool,
}

fn recorded(directory: &Path, name: &str) -> Result<Deployment> {
    Deployment::load(directory, name)?
        .context(ErrorCode::FileNotFound.error("no saved Cloudflare deployment for this --name"))
}

pub(crate) fn link(name: &str) -> Result<DeployedRelay> {
    link_in(&config::device_dir()?.join("cloudflare"), name)
}

pub(crate) fn list() -> Result<Vec<DeployedRelay>> {
    let directory = config::device_dir()?.join("cloudflare");
    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut relays = Vec::new();
    for entry in entries {
        let entry = entry?;
        let filename = entry.file_name();
        let Some(name) = filename
            .to_str()
            .and_then(|name| name.strip_prefix("deploy-"))
            .and_then(|name| name.strip_suffix(".toml"))
        else {
            continue;
        };
        if let Some(saved) = Deployment::load(&directory, name)?
            && saved.status == DeploymentStatus::Active
            && saved.url.is_some()
        {
            relays.push(link_in(&directory, name)?);
        }
    }
    relays.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(relays)
}

pub(super) fn link_in(directory: &Path, name: &str) -> Result<DeployedRelay> {
    let state = recorded(directory, name)?;
    if state.status != DeploymentStatus::Active {
        bail!(
            ErrorCode::InvalidConfig
                .error("this Cloudflare relay is being removed or has been removed")
        )
    }
    Ok(DeployedRelay {
        provider: "cloudflare",
        name: state.name,
        account_id: state.account_id,
        link: state.url.context(ErrorCode::InvalidConfig.error(
            "Cloudflare deployment did not finish; retry relay deploy --provider cloudflare",
        ))?,
    })
}

pub(crate) async fn remove(name: &str, no_browser: bool) -> Result<RemovedRelay> {
    state::validate_name(name)?;
    let directory = state::directory()?;
    let _lock = state::lock(&directory, "deployment")?;
    let state = recorded(&directory, name)?;
    if state.status == DeploymentStatus::Removed {
        return Ok(removed(name));
    }
    remove_with(Api::new(auth::token(no_browser).await?)?, &directory, name).await
}

pub(super) async fn remove_with(
    mut api: Api,
    directory: &Path,
    name: &str,
) -> Result<RemovedRelay> {
    let mut state = recorded(directory, name)?;
    if state.status == DeploymentStatus::Removed {
        return Ok(removed(name));
    }
    api.protect(&state.route);
    if let Some(worker) = api.worker(&state.account_id, name).await? {
        state.validate_ownership(&worker)?;
        let migration = worker["migration_tag"].as_str().unwrap_or_default();
        if migration != "v1" && !(migration == "v2" && state.status == DeploymentStatus::Removing) {
            bail!(
                ErrorCode::VersionMismatch
                    .error("unsupported Cloudflare Durable Object migration tag")
            )
        }
        state.status = DeploymentStatus::Removing;
        state.save(directory)?;
        let script = format!("/accounts/{}/workers/scripts/{name}", state.account_id);
        if migration == "v1" {
            let metadata = json!({
                "main_module":"relay.mjs", "compatibility_date":"2026-10-03",
                "tags":["xrun-relay",format!("xrun-deployment-{}",state.ownership)],
                "bindings":[],
                "migrations":{"old_tag":"v1","new_tag":"v2","deleted_classes":["XrunRelay"]}
            });
            api.upload(&script, &metadata, CLEANUP).await?;
        }
        api.json(Method::DELETE, &script, None).await?;
    }
    state.status = DeploymentStatus::Removed;
    state.url = None;
    state.save(directory)?;
    Ok(removed(name))
}

fn removed(name: &str) -> RemovedRelay {
    RemovedRelay {
        provider: "cloudflare",
        name: name.into(),
        removed: true,
        data_retained: false,
    }
}
