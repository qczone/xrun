//! User operations shared by CLI and desktop adapters.
//! This workspace API describes current-version behavior; errors retain machine codes.
#![deny(missing_docs)]
mod membership;
pub mod services;
mod settings;
mod status;

pub use membership::{
    Invitation, JoinInfo, Permission, Revocation, create_network, invite, join, pause_access,
    revoke, set_all_permissions, set_permission,
};
pub use settings::{ExecutionSettings, Settings, save_settings, settings};
pub use status::{
    LocalSnapshot, LocalStatus, NetworkStatus, Status, local_snapshot, local_status, status,
};
/// Read-only local task and file history. Queries never create or reset the database.
pub mod history {
    pub use crate::history::{FilePage, FileRecord, TaskOutput, TaskPage, files, output, tasks};
}

use crate::{config::Identity, error::ErrorCode, protocol::valid_name};
use anyhow::{Context, Result, bail};
pub(crate) fn user_name() -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| std::env::consts::OS.into());
    let mut value: String = host
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() {
                c
            } else {
                '-'
            }
        })
        .take(32)
        .collect();
    if value.is_empty() || !value.as_bytes()[0].is_ascii_lowercase() || !valid_name(&value) {
        value = format!("{}1", std::env::consts::OS)
    }
    value
}
pub(crate) fn validate_address(address: &str) -> Result<()> {
    let (_, port) = address
        .rsplit_once(':')
        .context(ErrorCode::InvalidAddress.error("an explicit port is required"))?;
    if port.parse::<u16>().ok().is_none_or(|p| p == 0) {
        bail!(ErrorCode::InvalidAddress.error("invalid port"))
    }
    let url = url::Url::parse(&format!("https://{address}"))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || matches!(url.host(), Some(url::Host::Ipv6(_)))
    {
        bail!(ErrorCode::InvalidAddress.error("expected IPv4 or hostname followed by :port"))
    };
    Ok(())
}
pub(crate) async fn join_identity(link: &str, name: Option<String>) -> Result<Identity> {
    let name = name
        .or_else(|| Identity::load().ok().map(|id| id.name))
        .unwrap_or_else(user_name);
    crate::network::join(link, name).await
}
