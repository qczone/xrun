//! Desktop-safe Cloudflare deployment, using the same native OAuth and Worker as CLI.
pub use crate::cloudflare::{Account, DeployedRelay};
use anyhow::Result;

/// Authorize in the browser when needed and list accounts without reading terminal
/// input. The callback receives the authorization URL for a desktop progress view.
/// `reauthorize` starts fresh browser consent instead of reusing cached credentials.
pub async fn cloudflare_accounts(
    prompt: &(dyn Fn(&str) + Send + Sync),
    reauthorize: bool,
) -> Result<Vec<Account>> {
    crate::cloudflare::accounts(prompt, reauthorize).await
}

/// Deploy the bundled Worker in an explicitly selected account. Existing ownership
/// and partial-deployment recovery checks match the CLI; no compiler is invoked.
pub async fn deploy_cloudflare(
    name: &str,
    account: String,
    prompt: &(dyn Fn(&str) + Send + Sync),
) -> Result<DeployedRelay> {
    crate::cloudflare::deploy_selected(name, account, prompt).await
}

/// Locally saved, completed, active deployments; this does not authorize or contact Cloudflare.
pub fn cloudflare_relays() -> Result<Vec<DeployedRelay>> {
    crate::cloudflare::list()
}

/// Reopen an OAuth address after validating its official Cloudflare host and path.
pub fn open_cloudflare_authorization(url: &str) -> Result<()> {
    crate::cloudflare::reopen_browser(url)
}
