//! Native Cloudflare authorization and deployment of the prebuilt relay.
mod api;
mod auth;
mod bundle;
mod deploy;
mod manage;
mod state;

pub(crate) use auth::reopen_browser;
pub(crate) use auth::{login, logout};
pub(crate) use deploy::deploy;
pub use deploy::{Account, DeployedRelay};
pub(crate) use deploy::{accounts, deploy_selected};
pub(crate) use manage::list;
pub(crate) use manage::{link, remove};

pub(crate) fn check(name: &str) -> anyhow::Result<serde_json::Value> {
    state::validate_name(name)?;
    let bundle = bundle::Bundle::embedded()?;
    Ok(serde_json::json!({"provider":"cloudflare", "name":name,
        "version":bundle.manifest.version, "sha256":bundle.manifest.sha256,
        "protocol":bundle.manifest.protocol, "bundled":true, "oauth_configured":auth::configured()}))
}

#[cfg(test)]
mod tests;
