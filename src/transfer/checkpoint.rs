//! Private, locked, expiring checkpoints. Every metadata file belongs to one existing intent.
use crate::{config, error::ErrorCode, protocol::now_ms};
use anyhow::{Context, Result, bail};
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    path::{Path, PathBuf},
};
pub(crate) const RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
pub(crate) struct Checkpoint<T> {
    pub state: Option<T>,
    pub blob: PathBuf,
    metadata: PathBuf,
    _lock: File,
}
impl<T: Serialize + DeserializeOwned> Checkpoint<T> {
    pub(crate) fn open(dir: &Path, key: &str) -> Result<Self> {
        if key.is_empty()
            || key.len() > 128
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!(ErrorCode::InvalidRequest.error("invalid transfer checkpoint identity"));
        }
        if std::fs::symlink_metadata(dir).is_ok_and(|m| !m.is_dir() || m.file_type().is_symlink()) {
            bail!(
                ErrorCode::InvalidPath
                    .error("transfer checkpoint directory is not private storage")
            );
        }
        std::fs::create_dir_all(dir)?;
        config::restrict_dir(dir)?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(format!("{key}.lock")))?;
        lock.try_lock()
            .context(ErrorCode::DeviceBusy.error("this transfer is already active"))?;
        let metadata = dir.join(format!("{key}.json"));
        let state = match std::fs::read(&metadata) {
            Ok(bytes) if bytes.len() <= 64 * 1024 => Some(serde_json::from_slice(&bytes)?),
            Ok(_) => bail!(ErrorCode::StorageError.error("transfer metadata exceeds limit")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            state,
            blob: dir.join(format!("{key}.blob")),
            metadata,
            _lock: lock,
        })
    }
    pub(crate) fn save(&mut self, state: T) -> Result<()> {
        config::atomic_private_write(&self.metadata, &serde_json::to_vec(&state)?)?;
        self.state = Some(state);
        Ok(())
    }
    pub(crate) fn clear(&mut self) -> Result<()> {
        for path in [&self.metadata, &self.blob] {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        self.state = None;
        Ok(())
    }
    pub(crate) fn adopt(&self, temp: tempfile::NamedTempFile) -> Result<()> {
        temp.as_file().sync_all()?;
        temp.persist_noclobber(&self.blob)
            .map_err(|error| error.error)?;
        #[cfg(windows)]
        config::private_acl(&self.blob, false)?;
        config::sync_parent(&self.blob)
    }
    pub(crate) fn new_blob(&self) -> Result<File> {
        let file = File::options()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&self.blob)?;
        #[cfg(windows)]
        config::private_acl(&self.blob, false)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(file)
    }
}
pub(crate) fn expire(dir: &Path, mut expired: impl FnMut(&str) -> Result<()>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        if entry.path().extension().is_none_or(|ext| ext != "json") || !entry.file_type()?.is_file()
        {
            continue;
        }
        let key = entry
            .path()
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let Ok(mut checkpoint) = Checkpoint::<serde_json::Value>::open(dir, &key) else {
            continue;
        };
        if checkpoint
            .state
            .as_ref()
            .and_then(|s| s["expires_at_ms"].as_i64())
            .is_some_and(|expiry| now_ms() >= expiry)
        {
            expired(&key)?;
            checkpoint.clear()?;
        }
    }
    Ok(())
}
