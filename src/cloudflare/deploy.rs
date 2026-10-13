use super::{
    api::Api,
    auth,
    bundle::Bundle,
    state::{self, Deployment, DeploymentStatus},
};
use crate::deployment::{self, DeploymentStage, ProgressDetails, ProgressStatus};
use crate::{crypto, error::ErrorCode};
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::Serialize;
use serde_json::json;
use std::{
    io::{IsTerminal, Write},
    path::Path,
};

#[derive(Clone, Serialize)]
/// A deployment recorded locally after the Worker and its HTTPS endpoint are ready.
pub struct DeployedRelay {
    /// Relay provider.
    pub provider: &'static str,
    /// Cloudflare Worker name.
    pub name: String,
    /// Owning Cloudflare account ID.
    pub account_id: String,
    /// Public relay address; it does not contain access credentials.
    pub link: String,
    /// Best-effort placement hint, applied only when creating new objects.
    pub region: super::Region,
}

/// Account offered by Cloudflare after authorization.
#[derive(Clone, Serialize)]
pub struct Account {
    /// Validated Cloudflare account ID.
    pub id: String,
    /// Account display name.
    pub name: String,
}

pub(crate) async fn accounts(
    prompt: &(dyn Fn(&str) + Send + Sync),
    reauthorize: bool,
    remember: bool,
) -> Result<Vec<Account>> {
    let token = if reauthorize || remember {
        auth::login_with_prompt(false, remember, Some(prompt)).await?
    } else {
        auth::token_with_prompt(false, Some(prompt)).await?
    };
    let api = Api::new(token)?;
    let result = account_list(&api).await;
    if result.is_err() {
        auth::finish_temporary(result).await
    } else {
        result
    }
}

pub(crate) async fn deploy(
    name: &str,
    account: Option<String>,
    no_browser: bool,
    region: Option<super::Region>,
) -> Result<DeployedRelay> {
    deploy_with_prompt(name, account, no_browser, None, region).await
}

pub(crate) async fn deploy_selected(
    name: &str,
    account: String,
    prompt: &(dyn Fn(&str) + Send + Sync),
    region: Option<super::Region>,
) -> Result<DeployedRelay> {
    deploy_with_prompt(name, Some(account), false, Some(prompt), region).await
}

pub(crate) async fn prepare_selected(
    name: &str,
    account: String,
    prompt: &(dyn Fn(&str) + Send + Sync),
    region: Option<super::Region>,
) -> Result<DeployedRelay> {
    deploy_operation(name, Some(account), false, Some(prompt), region).await
}

async fn deploy_with_prompt(
    name: &str,
    account: Option<String>,
    no_browser: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
    region: Option<super::Region>,
) -> Result<DeployedRelay> {
    let result = async {
        let relay = deploy_operation(name, account, no_browser, prompt, region).await?;
        crate::client::relay::check_relay(&relay.link).await?;
        Ok(relay)
    }
    .await;
    auth::finish_temporary(result).await
}

async fn deploy_operation(
    name: &str,
    account: Option<String>,
    no_browser: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
    region: Option<super::Region>,
) -> Result<DeployedRelay> {
    state::validate_name(name)?;
    let bundle = Bundle::embedded()?;
    let directory = state::directory()?;
    let _lock = state::lock(&directory, "deployment")?;
    let previous = Deployment::load(&directory, name)?;
    require_active(previous.as_ref())?;
    let account = account
        .or_else(|| std::env::var("CLOUDFLARE_ACCOUNT_ID").ok())
        .or_else(|| previous.as_ref().map(|state| state.account_id.clone()));
    if let Some(account) = &account {
        state::validate_account(account)?;
    }
    if let (Some(previous), Some(account)) = (&previous, &account)
        && previous.account_id != *account
    {
        bail!(ErrorCode::InvalidConfig.error(
            "this Worker name has a deployment record for another account; use a different name"
        ))
    }
    let api = Api::new(auth::token_with_prompt(no_browser, prompt).await?)?;
    let account = select_account(&api, account).await?;
    if !deployment::active() {
        eprintln!("[xrun] Deploying {name} to Cloudflare account {account}…");
    }
    let relay = deploy_with(api, &directory, name, &account, &bundle, region).await?;
    deployment::report(
        DeploymentStage::Deployed,
        ProgressStatus::Completed,
        ProgressDetails {
            deployment: Some(relay.clone()),
            next_command: Some(vec![
                "xrun".into(),
                "up".into(),
                "--relay".into(),
                relay.link.clone(),
            ]),
            ..Default::default()
        },
    );
    Ok(relay)
}

async fn select_account(api: &Api, selected: Option<String>) -> Result<String> {
    if let Some(account) = selected {
        return Ok(account);
    }
    let accounts = account_list(api).await?;
    let selected = if accounts.len() == 1 {
        &accounts[0]
    } else {
        for (index, account) in accounts.iter().enumerate() {
            if !deployment::active() {
                eprintln!("  {}. {} ({})", index + 1, account.name, account.id);
            }
        }
        deployment::report(
            DeploymentStage::AccountSelection,
            ProgressStatus::Waiting,
            ProgressDetails {
                accounts: Some(accounts.clone()),
                ..Default::default()
            },
        );
        if !std::io::stdin().is_terminal() || !deployment::interactive() {
            bail!(
                ErrorCode::InteractiveRequired
                    .error("multiple Cloudflare accounts; specify --account <ID>")
            )
        }
        eprint!("[xrun] Choose an account (1-{}): ", accounts.len());
        std::io::stderr().flush()?;
        let input = tokio::task::spawn_blocking(|| -> Result<String> {
            let mut input = String::new();
            std::io::stdin().read_line(&mut input)?;
            Ok(input)
        })
        .await??;
        let index = input
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|number| number.checked_sub(1));
        index
            .and_then(|index| accounts.get(index))
            .context(ErrorCode::InvalidConfig.error("invalid Cloudflare account selection"))?
    };
    Ok(selected.id.clone())
}

pub(super) async fn account_list(api: &Api) -> Result<Vec<Account>> {
    let mut accounts = Vec::new();
    for page in 1..=20 {
        let value = api
            .json(
                Method::GET,
                &format!("/accounts?per_page=50&page={page}"),
                None,
            )
            .await?;
        let rows = value.as_array().context("Cloudflare account list")?;
        for row in rows {
            let id = row["id"].as_str().context("Cloudflare account ID")?;
            state::validate_account(id)?;
            accounts.push(Account {
                id: id.into(),
                name: row["name"].as_str().unwrap_or("Cloudflare").into(),
            });
        }
        if rows.len() < 50 {
            break;
        }
    }
    if accounts.is_empty() {
        bail!(ErrorCode::Unauthenticated.error("no Cloudflare account is available"))
    }
    Ok(accounts)
}

pub(super) async fn deploy_with(
    api: Api,
    directory: &Path,
    name: &str,
    account: &str,
    bundle: &Bundle,
    region: Option<super::Region>,
) -> Result<DeployedRelay> {
    state::validate_name(name)?;
    state::validate_account(account)?;
    let previous = Deployment::load(directory, name)?;
    require_active(previous.as_ref())?;
    if previous
        .as_ref()
        .is_some_and(|state| state.account_id != account)
    {
        bail!(ErrorCode::InvalidConfig.error("deployment account differs from the saved record"))
    }
    // Durable Object uploads require workers.dev to be registered already.
    let subdomain = subdomain(&api, account).await?;
    let script = format!("/accounts/{account}/workers/scripts/{name}");
    let published = api.worker(account, name).await?;
    let mut state = deployment::step(DeploymentStage::WorkerCheck, async {
        Ok(match (previous, &published) {
            (None, Some(_)) => bail!(
                ErrorCode::AlreadyExists
                    .error("a Worker with this name already exists; choose another --name")
            ),
            (Some(state), Some(published)) => {
                state.validate_ownership(published)?;
                if published["migration_tag"] != "v1" && published["migration_tag"] != "v2" {
                    bail!(
                        ErrorCode::VersionMismatch
                            .error("unsupported Cloudflare Durable Object migration tag")
                    )
                }
                state
            }
            (Some(state), None) => state,
            (None, None) => Deployment {
                name: name.into(),
                account_id: account.into(),
                ownership: crypto::random_token(),
                url: None,
                status: DeploymentStatus::Active,
                region: super::Region::Auto,
            },
        })
    })
    .await?;
    if let Some(region) = region {
        state.region = region;
    }
    // Persist before upload: a timeout after a successful upload must be retryable
    // with the same ownership marker.
    state.save(directory)?;
    let mut metadata = json!({
        "main_module": "relay.mjs",
        "compatibility_date": bundle.manifest.compatibility_date,
        "tags": ["xrun-relay", format!("xrun-deployment-{}", state.ownership)],
        "bindings": [
            {"type":"durable_object_namespace", "name":"NETWORKS", "class_name":"XrunRelay"},
            {"type":"durable_object_namespace", "name":"DATA", "class_name":"XrunData"},
            {"type":"json", "name":"XRUN_PROTOCOL_MIN", "json":bundle.manifest.protocol.min},
            {"type":"json", "name":"XRUN_PROTOCOL_MAX", "json":bundle.manifest.protocol.max}
        ]
    });
    if state.region != super::Region::Auto {
        metadata["bindings"]
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"plain_text", "name":"XRUN_RELAY_REGION", "text":state.region}));
    }
    if published.is_none() {
        metadata["migrations"] =
            json!({"new_tag":"v2", "new_sqlite_classes":["XrunRelay","XrunData"]});
    } else if published
        .as_ref()
        .is_some_and(|worker| worker["migration_tag"] == "v1")
    {
        metadata["migrations"] =
            json!({"old_tag":"v1", "new_tag":"v2", "new_sqlite_classes":["XrunData"]});
    }
    api.upload(&script, &metadata, &bundle.source).await?;
    api.json(
        Method::POST,
        &format!("{script}/subdomain"),
        Some(json!({"enabled":true, "previews_enabled":false})),
    )
    .await?;
    let url = format!("https://{name}.{subdomain}.workers.dev");
    let state = Deployment {
        url: Some(url.clone()),
        ..state
    };
    state.save(directory)?;
    Ok(DeployedRelay {
        provider: "cloudflare",
        name: name.into(),
        account_id: account.into(),
        link: url,
        region: state.region,
    })
}

fn require_active(state: Option<&Deployment>) -> Result<()> {
    if state.is_some_and(|state| state.status != DeploymentStatus::Active) {
        bail!(ErrorCode::InvalidConfig.error(
            "this Cloudflare relay is being removed or has been removed; deploy with a new --name"
        ))
    }
    Ok(())
}

async fn subdomain(api: &Api, account: &str) -> Result<String> {
    let path = format!("/accounts/{account}/workers/subdomain");
    let value = match api.workers_subdomain(account).await? {
        Some(value)
            if value["subdomain"]
                .as_str()
                .is_some_and(|name| !name.is_empty()) =>
        {
            value
        }
        _ => {
            let name = format!("xrun-{}", &account[..12]);
            if !deployment::active() {
                eprintln!("[xrun] Registering {name}.workers.dev for this Cloudflare account…");
            }
            api.json(Method::PUT, &path, Some(json!({"subdomain":name})))
                .await?
        }
    };
    let name = value["subdomain"]
        .as_str()
        .context("Cloudflare workers.dev subdomain")?;
    state::validate_name(name)?;
    Ok(name.to_owned())
}
