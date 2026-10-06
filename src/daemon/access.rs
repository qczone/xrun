//! One authorization snapshot and change detector for all encrypted sessions.
use super::Runtime;
use crate::{
    config::{DaemonConfig, Identity},
    error::ErrorCode,
    membership::SignedRoster,
};
use anyhow::{Result, bail};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

const ACCESS_SCAN_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone)]
pub(super) struct Authorization {
    pub config: DaemonConfig,
    pub identity: Identity,
    pub roster: SignedRoster,
    pub files: Option<(Fingerprint, Fingerprint)>,
}
pub(super) type Snapshot = Result<Arc<Authorization>, Arc<str>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fingerprint {
    modified: SystemTime,
    bytes: u64,
    file_id: (u64, u64),
}
fn fingerprint(path: &Path) -> Result<Fingerprint> {
    let metadata = std::fs::metadata(path)?;
    #[cfg(unix)]
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let file_id = (
        metadata
            .created()?
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_nanos() as u64,
        0,
    );
    Ok(Fingerprint {
        modified: metadata.modified()?,
        bytes: metadata.len(),
        file_id,
    })
}

fn scan(rt: &Runtime, dir: &Path) -> Result<Option<Arc<Authorization>>> {
    let files = (
        fingerprint(&dir.join("daemon.toml"))?,
        fingerprint(&dir.join("identity.toml"))?,
    );
    let roster = rt.members.load(&rt.network_id)?;
    let unchanged = rt
        .access
        .borrow()
        .as_ref()
        .is_ok_and(|value| value.files.as_ref() == Some(&files))
        && rt
            .access
            .borrow()
            .as_ref()
            .is_ok_and(|value| value.roster.signature == roster.signature);
    if unchanged {
        return Ok(None);
    }
    let identity = Identity::load()?;
    if identity.device_id != rt.id.device_id || identity.network != rt.id.network {
        bail!(ErrorCode::IdentityChanged.error("restart daemon after replacing its identity"));
    }
    let config = DaemonConfig::load()?;
    Ok(Some(Arc::new(Authorization {
        config,
        identity,
        roster,
        files: Some(files),
    })))
}

pub(super) async fn monitor(rt: Arc<Runtime>, dir: PathBuf) -> Result<()> {
    let mut interval = tokio::time::interval(ACCESS_SCAN_INTERVAL);
    let mut roster_changes = rt.members.subscribe();
    let mut reloads = rt.control.reload_requests();
    loop {
        let reply = tokio::select! {
            _ = interval.tick() => None,
            _ = roster_changes.changed() => None,
            reply = reloads.recv() => reply,
        };
        let result = refresh(rt.clone(), dir.clone()).await;
        if let Some(reply) = reply {
            let _ = reply.send(result.map_err(|error| format!("{error:#}")));
        }
    }
}
fn refresh_blocking(rt: &Runtime, dir: &Path) -> Result<()> {
    let _guard = rt.access_scan.lock().unwrap();
    let result = scan(rt, dir);
    match &result {
        Ok(Some(snapshot)) => {
            let _ = rt.access.send_replace(Ok(snapshot.clone()));
        }
        Ok(None) => {}
        Err(error) => {
            let message = format!("{error:#}");
            if rt.access.borrow().as_ref().err().map(AsRef::as_ref) != Some(message.as_str()) {
                let _ = rt.access.send_replace(Err(message.into()));
            }
        }
    }
    result.map(|_| ())
}
pub(super) async fn refresh(rt: Arc<Runtime>, dir: PathBuf) -> Result<()> {
    tokio::task::spawn_blocking(move || refresh_blocking(&rt, &dir)).await?
}

impl Runtime {
    pub(super) fn authorization(&self) -> Result<Arc<Authorization>> {
        self.access
            .borrow()
            .clone()
            .map_err(|message| anyhow::anyhow!(ErrorCode::StorageError.error(message.to_string())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_detects_atomic_replacement_even_with_equal_length() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.toml");
        crate::config::atomic_private_write(&path, b"first")?;
        let before = fingerprint(&path)?;
        crate::config::atomic_private_write(&path, b"other")?;
        assert_ne!(before, fingerprint(&path)?);
        Ok(())
    }
}
