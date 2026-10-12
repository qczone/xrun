//! User operations shared by CLI and desktop adapters.
//! This workspace API describes current-version behavior; errors retain machine codes.
#![deny(missing_docs)]
mod membership;
pub mod relay;
pub mod remote;
pub mod services;
mod settings;
mod status;

pub use membership::{
    ConnectionKind, Invitation, JoinInfo, Permission, Revocation, connect, connection_kind,
    create_network, invite, join, pause_access, revoke, set_all_permissions, set_permission,
};
pub use settings::{
    ExecutionSettings, Settings, save_attachment_retention, save_settings, settings,
};
pub use status::{
    LocalSnapshot, LocalStatus, NetworkStatus, Status, device_status, local_snapshot, local_status,
    status,
};
/// Query the current relay's persisted ciphertext counters. Managers see their
/// network, other members see only their own device. No service is started.
pub async fn traffic(
    query: crate::protocol::TrafficQuery,
) -> Result<crate::protocol::TrafficReport> {
    query.validate()?;
    let mut id = Identity::load()?;
    crate::net::renew_identity(&mut id).await?;
    crate::network::traffic(&id, query).await
}
/// Local activity and task output. Reads never create or reset the database;
/// activity reads can clean expired attachment cache copies.
pub mod history {
    pub use crate::attachments::{Attachment, AttachmentMetadata, AttachmentPreview};
    pub use crate::history::{ActivityPage, activity, attachment, job, save_attachment};
    pub use crate::history::{JobOutput, output};
}

use crate::{config::Identity, error::ErrorCode, protocol::valid_name};
use anyhow::{Context, Result, bail};
/// Suggested editable device name derived from the local hostname.
pub fn suggested_name() -> String {
    user_name()
}
pub(crate) fn user_name() -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .filter(|host| !host.is_empty())
        .or_else(system_hostname)
        .unwrap_or_else(|| std::env::consts::OS.into());
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
#[cfg(unix)]
fn system_hostname() -> Option<String> {
    let mut bytes = [0u8; 256];
    // gethostname writes at most the supplied buffer length. Do not assume a
    // terminating NUL when a platform returns a truncated name.
    if unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return None;
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8(bytes[..end].to_vec()).ok()
}
#[cfg(not(unix))]
fn system_hostname() -> Option<String> {
    None
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
