//! Local authority administration. These actions are never endpoint business requests.
use crate::{config, membership::backup};
use anyhow::Result;
pub use backup::BackupInfo;
use std::path::Path;

/// Encrypt a consistent manager snapshot without stopping the service. Existing files
/// are never overwritten; the password belongs in a prompt or protected local UI.
pub fn backup_authority(path: &Path, password: &[u8]) -> Result<BackupInfo> {
    backup::export_at(&config::device_dir()?, path, password, None)
}
/// Prepare an encrypted handover to an existing trusted member. The old manager
/// remains active until that member imports and publishes the new authority generation.
pub fn migrate_authority(device: &str, path: &Path, password: &[u8]) -> Result<BackupInfo> {
    backup::export_at(&config::device_dir()?, path, password, Some(device))
}
/// Validate and atomically restore identity, root keys, authority DB and local policy.
/// Requires a stopped daemon; wrong passwords and failed validation preserve its state.
pub fn restore_authority(path: &Path, password: &[u8]) -> Result<BackupInfo> {
    backup::import_at(&config::device_dir()?, path, password)
}
/// Finish a verified handover by revoking saved Cloudflare credentials and removing
/// every managed plaintext root-key copy before this host accepts ordinary operations.
pub async fn retire_authority() -> Result<()> {
    let identity = config::Identity::load()?;
    let roster = crate::network::current(&identity)?;
    if roster.roster.manager_id == identity.device_id {
        anyhow::bail!(
            crate::error::ErrorCode::ManagerProtected
                .error("complete the handover on the new manager first")
        );
    }
    crate::cloudflare::logout().await?;
    backup::retire_at(&config::device_dir()?, &roster, &identity)?;
    crate::control::refresh_access().await
}
