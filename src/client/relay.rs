//! Desktop-safe Cloudflare deployment, using the same native OAuth and Worker as CLI.
pub use crate::cloudflare::{Account, DeployedRelay, Region};
pub use crate::deployment::{
    DeploymentProgress, DeploymentReporter, DeploymentStage, ProgressDetails, ProgressStatus,
};
use anyhow::Result;

/// Authorize in the browser when needed and list accounts without reading terminal
/// input. The callback receives the authorization URL for a desktop progress view.
/// `reauthorize` starts fresh browser consent instead of reusing cached credentials.
pub async fn cloudflare_accounts(
    prompt: &(dyn Fn(&str) + Send + Sync),
    reauthorize: bool,
    remember: bool,
) -> Result<Vec<Account>> {
    crate::cloudflare::accounts(prompt, reauthorize, remember).await
}

/// Deploy the bundled Worker in an explicitly selected account. Existing ownership
/// and partial-deployment recovery checks match the CLI; no compiler is invoked.
pub async fn deploy_cloudflare(
    name: &str,
    account: String,
    prompt: &(dyn Fn(&str) + Send + Sync),
    region: Option<Region>,
) -> Result<DeployedRelay> {
    crate::cloudflare::deploy_selected(name, account, prompt, region).await
}

/// Prepare a Worker for a desktop connection. The caller must always finish the
/// authorization session, including when creation or service startup fails.
pub async fn prepare_cloudflare_relay(
    name: &str,
    account: String,
    prompt: &(dyn Fn(&str) + Send + Sync),
    region: Option<Region>,
) -> Result<DeployedRelay> {
    crate::cloudflare::deploy::prepare_selected(name, account, prompt, region).await
}

/// Read a saved, completed deployment for an explicit retry without another upload.
pub fn saved_cloudflare_relay(name: &str) -> Result<DeployedRelay> {
    crate::cloudflare::link(name)
}

/// Wait at most 30 seconds for the normal relay protocol handshake. This does not
/// create a network or claim the relay, and retains certificate and protocol checks.
pub async fn check_relay(link: &str) -> Result<()> {
    crate::network::validate_connection_link(link)?;
    crate::deployment::step(DeploymentStage::RelayCheck, async {
        let ready = async {
            let mut delay = std::time::Duration::from_millis(250);
            loop {
                match crate::network::check(link).await {
                    Ok(()) => return Ok(()),
                    Err(error)
                        if crate::net::explicit(&error)
                            || crate::error::code(&error).is_some_and(|code| !code.is_network()) =>
                    {
                        return Err(error);
                    }
                    Err(_) => {
                        tokio::time::sleep(delay).await;
                        delay = (delay * 2).min(std::time::Duration::from_secs(2));
                    }
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), ready).await.map_err(|_| {
            crate::error::ErrorCode::ConnectTimeout.error(
                "relay was deployed but is not reachable yet; use relay check or retry with the saved relay")
        })?
    }).await
}

/// Locally saved, completed, active deployments; this does not authorize or contact Cloudflare.
pub fn cloudflare_relays() -> Result<Vec<DeployedRelay>> {
    crate::cloudflare::list()
}

/// Reopen an OAuth address after validating its official Cloudflare host and path.
pub fn open_cloudflare_authorization(url: &str) -> Result<()> {
    crate::cloudflare::reopen_browser(url)
}

/// Revoke temporary or remembered authorization before deleting stored credentials.
pub async fn logout_cloudflare() -> Result<()> {
    crate::cloudflare::logout().await
}
/// Locally selected login preference; it does not assert provider token validity.
pub fn cloudflare_remembered_login() -> Result<bool> {
    crate::cloudflare::auth::remembered_login()
}
/// End a desktop temporary session, for example when cancelling account selection.
pub async fn end_cloudflare_authorization() -> Result<()> {
    crate::cloudflare::auth::finish_temporary(Ok(())).await
}
/// Cancel only a pending OAuth flow; a started deployment keeps its durable recovery state.
pub fn cancel_pending_cloudflare_authorization() -> bool {
    crate::cloudflare::auth::cancel_pending_login()
}
