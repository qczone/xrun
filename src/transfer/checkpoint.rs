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
        match temp.persist_noclobber(&self.blob) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::CrossesDevices => {
                // System temporary storage can be a separate mount. Copy the immutable
                // snapshot to private staging on the checkpoint filesystem, then publish
                // it atomically without replacing an existing checkpoint.
                let mut source = error.file;
                std::io::Seek::rewind(source.as_file_mut())?;
                let mut staged = tempfile::NamedTempFile::new_in(
                    self.blob.parent().context("checkpoint directory missing")?,
                )?;
                std::io::copy(source.as_file_mut(), staged.as_file_mut())?;
                staged.as_file().sync_all()?;
                staged
                    .persist_noclobber(&self.blob)
                    .map_err(|error| error.error)?;
            }
            Err(error) => return Err(error.error.into()),
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn adoption_preserves_an_existing_checkpoint() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let checkpoint = Checkpoint::<serde_json::Value>::open(dir.path(), "existing")?;
        std::fs::write(&checkpoint.blob, b"original")?;
        let mut snapshot = tempfile::NamedTempFile::new_in(dir.path())?;
        snapshot.write_all(b"replacement")?;
        assert!(checkpoint.adopt(snapshot).is_err());
        assert_eq!(std::fs::read(&checkpoint.blob)?, b"original");
        Ok(())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn adoption_crosses_filesystems_without_truncating_or_overwriting() -> Result<()> {
        use std::io::{Seek, SeekFrom};
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir()?;
        let shared_memory = Path::new("/dev/shm");
        if !shared_memory.is_dir()
            || std::fs::metadata(dir.path())?.dev() == std::fs::metadata(shared_memory)?.dev()
        {
            eprintln!("cross-filesystem fixture requires a separate /dev/shm mount");
            return Ok(());
        }
        let checkpoint = Checkpoint::<serde_json::Value>::open(dir.path(), "cross-device")?;
        let content: Vec<_> = (0..128 * 1024).map(|index| (index % 251) as u8).collect();
        let mut snapshot = tempfile::NamedTempFile::new_in(shared_memory)?;
        snapshot.write_all(&content)?;
        snapshot.seek(SeekFrom::Start(7))?;
        let source_path = snapshot.path().to_owned();
        checkpoint.adopt(snapshot)?;
        assert_eq!(std::fs::read(&checkpoint.blob)?, content);
        assert_eq!(
            std::fs::metadata(&checkpoint.blob)?.permissions().mode() & 0o777,
            0o600
        );
        assert!(!source_path.exists());

        let mut replacement = tempfile::NamedTempFile::new_in(shared_memory)?;
        replacement.write_all(b"replacement")?;
        let replacement_path = replacement.path().to_owned();
        assert!(checkpoint.adopt(replacement).is_err());
        assert_eq!(std::fs::read(&checkpoint.blob)?, content);
        assert!(!replacement_path.exists());
        assert_eq!(std::fs::read_dir(dir.path())?.count(), 2);
        Ok(())
    }
}
