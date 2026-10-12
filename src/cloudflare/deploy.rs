use super::{
    api::Api,
    auth,
    bundle::Bundle,
    state::{self, Deployment, DeploymentStatus},
};
use crate::{crypto, error::ErrorCode};
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde::Serialize;
use serde_json::json;
use std::{
    io::{IsTerminal, Write},
    path::Path,
};

#[derive(Serialize)]
/// A deployment recorded locally after the Worker and its private route are ready.
pub struct DeployedRelay {
    /// Relay provider.
    pub provider: &'static str,
    /// Cloudflare Worker name.
    pub name: String,
    /// Owning Cloudflare account ID.
    pub account_id: String,
    /// Complete private relay deployment link.
    pub link: String,
}

/// Account offered by Cloudflare after authorization.
#[derive(Serialize)]
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
) -> Result<DeployedRelay> {
    deploy_with_prompt(name, account, no_browser, None).await
}

pub(crate) async fn deploy_selected(
    name: &str,
    account: String,
    prompt: &(dyn Fn(&str) + Send + Sync),
) -> Result<DeployedRelay> {
    deploy_with_prompt(name, Some(account), false, Some(prompt)).await
}

async fn deploy_with_prompt(
    name: &str,
    account: Option<String>,
    no_browser: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
) -> Result<DeployedRelay> {
    let result = deploy_operation(name, account, no_browser, prompt).await;
    auth::finish_temporary(result).await
}

async fn deploy_operation(
    name: &str,
    account: Option<String>,
    no_browser: bool,
    prompt: Option<&(dyn Fn(&str) + Send + Sync)>,
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
    eprintln!("[xrun] Deploying {name} to Cloudflare account {account}…");
    deploy_with(api, &directory, name, &account, &bundle).await
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
            eprintln!("  {}. {} ({})", index + 1, account.name, account.id);
        }
        if !std::io::stdin().is_terminal() {
            bail!(
                ErrorCode::InteractiveRequired
                    .error("multiple Cloudflare accounts; specify --account <ID>")
            )
        }
        eprint!("[xrun] Choose an account (1-{}): ", accounts.len());
        std::io::stderr().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
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
    mut api: Api,
    directory: &Path,
    name: &str,
    account: &str,
    bundle: &Bundle,
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
    let script = format!("/accounts/{account}/workers/scripts/{name}");
    let published = api.worker(account, name).await?;
    let state = match (previous, &published) {
        (None, Some(_)) => bail!(
            ErrorCode::AlreadyExists
                .error("a Worker with this name already exists; choose another --name")
        ),
        (Some(state), Some(published)) => {
            state.validate_ownership(published)?;
            if published["migration_tag"] != "v1" {
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
            route: crypto::random_token(),
            ownership: crypto::random_token(),
            url: None,
            status: DeploymentStatus::Active,
        },
    };
    api.protect(&state.route);
    // Persist before upload: a timeout after a successful upload must be retryable
    // with the same private route and ownership marker.
    state.save(directory)?;
    let mut metadata = json!({
        "main_module": "relay.mjs",
        "compatibility_date": bundle.manifest.compatibility_date,
        "tags": ["xrun-relay", format!("xrun-deployment-{}", state.ownership)],
        "bindings": [
            {"type":"durable_object_namespace", "name":"NETWORKS", "class_name":"XrunRelay"},
            {"type":"secret_text", "name":"RELAY_ROUTE", "text":state.route},
            {"type":"json", "name":"XRUN_PROTOCOL_MIN", "json":bundle.manifest.protocol.min},
            {"type":"json", "name":"XRUN_PROTOCOL_MAX", "json":bundle.manifest.protocol.max}
        ]
    });
    if published.is_none() {
        metadata["migrations"] = json!({"new_tag":"v1", "new_sqlite_classes":["XrunRelay"]});
    }
    api.upload(&script, &metadata, &bundle.source).await?;
    let subdomain = subdomain(&api, account).await?;
    api.json(
        Method::POST,
        &format!("{script}/subdomain"),
        Some(json!({"enabled":true, "previews_enabled":false})),
    )
    .await?;
    let url = format!("https://{name}.{subdomain}.workers.dev/{}", state.route);
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
    let value = match api.optional(&path).await? {
        Some(value)
            if value["subdomain"]
                .as_str()
                .is_some_and(|name| !name.is_empty()) =>
        {
            value
        }
        _ => {
            let name = format!("xrun-{}", &account[..12]);
            eprintln!("[xrun] Registering {name}.workers.dev for this Cloudflare account…");
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
