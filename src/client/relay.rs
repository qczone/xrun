//! Desktop-safe Cloudflare deployment, using the same native OAuth and Worker as CLI.
pub use crate::cloudflare::{Account, DeployedRelay, Region};
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
