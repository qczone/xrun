//! One authorization snapshot and change detector for all encrypted sessions.
use super::Runtime;
use crate::{
    config::{DaemonConfig, Identity},
    error::{CodedError, ErrorCode},
    membership::SignedRoster,
};
use anyhow::{Result, bail};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

const ACCESS_SCAN_INTERVAL: Duration = Duration::from_secs(2);
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(60);

fn watch_directory(
    dir: &Path,
    changes: tokio::sync::mpsc::Sender<()>,
) -> notify::Result<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let relevant = match event {
            Ok(event) => {
                !matches!(event.kind, EventKind::Access(_))
                    && event.paths.iter().any(|path| {
                        path.file_name().is_some_and(|name| {
                            ["daemon.toml", "identity.toml", "roster.db", "roster.db-wal"]
                                .iter()
                                .any(|candidate| name == *candidate)
                        }) || matches!(event.kind, EventKind::Remove(_)) && path.is_dir()
                    })
            }
            Err(error) => {
                tracing::warn!(%error, "authorization watcher failed; polling remains enabled");
                true
            }
        };
        if relevant {
            // One queued signal coalesces editor writes, WAL changes and renames.
            let _ = changes.try_send(());
        }
    })?;
    // Watch the directory, not the old inode replaced by atomic_private_write.
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

#[derive(Clone)]
pub(super) struct Authorization {
    pub config: DaemonConfig,
    pub identity: Identity,
    pub roster: SignedRoster,
    pub files: Option<(Fingerprint, Fingerprint)>,
}
pub(super) type Snapshot = Result<Arc<Authorization>, Arc<CodedError>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Fingerprint {
    modified: SystemTime,
    bytes: u64,
    file_id: (u64, u64),
}
fn fingerprint(path: &Path) -> Result<Fingerprint> {
    #[cfg(unix)]
    let metadata = std::fs::metadata(path)?;
    #[cfg(unix)]
    let file_id = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(windows)]
    let (metadata, file_id) = {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        // Metadata and identity must come from the same open file. Its default
        // Windows share flags allow the CLI to atomically replace the pathname.
        let file = std::fs::File::open(path)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        (
            file.metadata()?,
            (
                u64::from(info.dwVolumeSerialNumber),
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            ),
        )
    };
    Ok(Fingerprint {
        modified: metadata.modified()?,
        bytes: metadata.len(),
        file_id,
    })
}

fn scan(rt: &Runtime, dir: &Path, force: bool) -> Result<Option<Arc<Authorization>>> {
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
    if unchanged && !force {
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
    let (notify, mut file_changes) = tokio::sync::mpsc::channel(1);
    let _watcher = watch_directory(&dir, notify)
        .map_err(|error| {
            tracing::warn!(%error, "authorization watcher unavailable; polling remains enabled");
            error
        })
        .ok();
    let mut debounce = None;
    loop {
        let deadline =
            debounce.unwrap_or_else(|| tokio::time::Instant::now() + ACCESS_SCAN_INTERVAL);
        let reply = tokio::select! {
            _ = interval.tick() => None,
            _ = roster_changes.changed() => None,
            reply = reloads.recv() => reply,
            Some(()) = file_changes.recv() => {
                debounce.get_or_insert_with(|| tokio::time::Instant::now() + CHANGE_DEBOUNCE);
                continue;
            },
            _ = tokio::time::sleep_until(deadline), if debounce.is_some() => None,
        };
        debounce = None;
        let result = refresh(rt.clone(), dir.clone(), reply.is_some()).await;
        if let Some(reply) = reply {
            let _ = reply.send(result);
        }
    }
}
fn refresh_blocking(rt: &Runtime, dir: &Path, force: bool) -> Result<()> {
    let _guard = rt.access_scan.lock().unwrap();
    let result = scan(rt, dir, force);
    match &result {
        Ok(Some(snapshot)) => {
            let _ = rt.access.send_replace(Ok(snapshot.clone()));
        }
        Ok(None) => {}
        Err(error) => {
            let (code, message) = crate::error::wire(error);
            let failure = Arc::new(CodedError::from_wire(code, message));
            if rt.access.borrow().as_ref().err() != Some(&failure) {
                let _ = rt.access.send_replace(Err(failure));
            }
        }
    }
    result.map(|_| ())
}
pub(super) async fn refresh(rt: Arc<Runtime>, dir: PathBuf, force: bool) -> Result<()> {
    tokio::task::spawn_blocking(move || refresh_blocking(&rt, &dir, force)).await?
}

impl Runtime {
    pub(super) fn authorization(&self) -> Result<Arc<Authorization>> {
        self.access
            .borrow()
            .clone()
            .map_err(|failure| anyhow::anyhow!(failure.as_ref().clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn parent_watcher_observes_atomic_replacements_without_watching_old_inodes() -> Result<()>
    {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.toml");
        crate::config::atomic_private_write(&path, b"first")?;
        let (sender, mut changes) = tokio::sync::mpsc::channel(1);
        let _watcher = watch_directory(dir.path(), sender)?;
        for bytes in [b"other", b"third"] {
            crate::config::atomic_private_write(&path, bytes)?;
            tokio::time::timeout(Duration::from_secs(4), changes.recv())
                .await?
                .ok_or_else(|| anyhow::anyhow!("watcher closed"))?;
        }
        Ok(())
    }

    #[test]
    fn fingerprint_detects_replacement_with_equal_length_and_timestamps() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.toml");
        crate::config::atomic_private_write(&path, b"first")?;
        let metadata = std::fs::metadata(&path)?;
        let before = fingerprint(&path)?;
        crate::config::atomic_private_write(&path, b"other")?;
        let replacement = std::fs::OpenOptions::new().write(true).open(&path)?;
        replacement.set_times(std::fs::FileTimes::new().set_modified(metadata.modified()?))?;
        #[cfg(windows)]
        {
            use std::os::windows::{fs::MetadataExt, io::AsRawHandle};
            use windows_sys::Win32::{Foundation::FILETIME, Storage::FileSystem::SetFileTime};
            let created = metadata.creation_time();
            let created = FILETIME {
                dwLowDateTime: created as u32,
                dwHighDateTime: (created >> 32) as u32,
            };
            if unsafe {
                SetFileTime(
                    replacement.as_raw_handle() as _,
                    &created,
                    std::ptr::null(),
                    std::ptr::null(),
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        drop(replacement);
        let after = fingerprint(&path)?;
        assert_eq!(before.modified, after.modified);
        assert_eq!(before.bytes, after.bytes);
        assert_ne!(before.file_id, after.file_id);
        Ok(())
    }
}
