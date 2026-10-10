//! Independent, private snapshots of transferred files and screenshots.
use crate::{config, error::ErrorCode, protocol::now_ms};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

const DAY_MS: i64 = 86_400_000;
const PREVIEW_LIMIT: u64 = 8 * 1024 * 1024;
const TEXT_LIMIT: u64 = 1024 * 1024;
const CLEANUP_INTERVAL_MS: i64 = 10 * 60 * 1000;

/// Immutable description saved with the activity summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentMetadata {
    /// Opaque cache identifier, independent of the original path.
    pub id: String,
    /// Suggested filename when saving a copy.
    pub name: String,
    /// Snapshot size in bytes.
    pub size: u64,
    /// SHA-256 of the exact snapshot bytes.
    pub sha256: String,
    /// Snapshot creation time in Unix milliseconds.
    pub created_at_ms: i64,
}

/// Current attachment availability under the saved retention policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// Persisted description of the original snapshot.
    #[serde(flatten)]
    pub metadata: AttachmentMetadata,
    /// Available, expired, or missing; the activity summary is retained in all cases.
    pub status: String,
    /// Expiry under the current policy, or none for indefinite retention.
    pub expires_at_ms: Option<i64>,
}
/// A bounded inline preview; other formats can be saved using the native dialog.
#[derive(Serialize)]
pub struct AttachmentPreview {
    /// Current availability and snapshot details.
    pub attachment: Attachment,
    /// PNG or JPEG data URL, never executable document markup.
    pub image: Option<String>,
    /// Complete UTF-8 text for files of at most one MiB, rendered as plain text.
    pub text: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Cache {
    dir: PathBuf,
    database: PathBuf,
}

impl Cache {
    pub(crate) fn new(data_dir: &Path) -> Self {
        Self::for_database(&data_dir.join("daemon.db"))
    }
    pub(crate) fn for_database(path: &Path) -> Self {
        Self {
            dir: path.parent().expect("database parent").join("attachments"),
            database: path.to_owned(),
        }
    }
    fn data_dir(&self) -> &Path {
        self.dir
            .parent()
            .expect("attachment directory has a parent")
    }
    pub(crate) fn retention_days(&self) -> Result<u16> {
        Ok(config::DaemonConfig::load_at(self.data_dir())?.attachment_retention_days)
    }
    fn check_directory(&self) -> Result<()> {
        match std::fs::symlink_metadata(&self.dir) {
            Ok(metadata) if metadata.is_dir() => Ok(()),
            Ok(_) => bail!(ErrorCode::InvalidPath.error("attachment cache must be a directory")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
    fn path(&self, id: &str) -> Result<PathBuf> {
        created_at(id).context(ErrorCode::InvalidPath.error("invalid attachment identifier"))?;
        self.check_directory()?;
        Ok(self.dir.join(format!("{id}.blob")))
    }
    pub(crate) fn save_file(&self, path: &Path, name: &str) -> Result<AttachmentMetadata> {
        self.save(File::open(path)?, name)
    }
    pub(crate) fn save(&self, mut input: impl Read, name: &str) -> Result<AttachmentMetadata> {
        // The caller supplies the immutable transfer/capture snapshot, never a
        // mutable published destination. Filenames never influence cache paths.
        self.check_directory()?;
        std::fs::create_dir_all(&self.dir)?;
        config::restrict_dir(&self.dir)?;
        let created_at_ms = now_ms();
        let id = format!("{created_at_ms}_{}", uuid::Uuid::new_v4().simple());
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir)?;
        let mut hash = Sha256::new();
        let mut size = 0_u64;
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = input.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            size += count as u64;
            temp.write_all(&buffer[..count])?;
            hash.update(&buffer[..count]);
        }
        temp.as_file().sync_all()?;
        let path = self.path(&id)?;
        temp.persist_noclobber(&path).map_err(|error| error.error)?;
        #[cfg(windows)]
        config::private_acl(&path, false)?;
        config::sync_parent(&path)?;
        let name: String = name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("attachment")
            .chars()
            .filter(|ch| !ch.is_control())
            .take(240)
            .collect();
        Ok(AttachmentMetadata {
            id,
            name: if name.is_empty() {
                "attachment".into()
            } else {
                name
            },
            size,
            sha256: hex::encode(hash.finalize()),
            created_at_ms,
        })
    }
    pub(crate) fn describe(
        &self,
        metadata: AttachmentMetadata,
        days: u16,
        now: i64,
    ) -> Result<Attachment> {
        let expires_at_ms = (days != 0).then(|| {
            metadata
                .created_at_ms
                .saturating_add(i64::from(days) * DAY_MS)
        });
        let status = if expires_at_ms.is_some_and(|expiry| now >= expiry) {
            "expired"
        } else {
            match std::fs::symlink_metadata(self.path(&metadata.id)?) {
                Ok(file) if file.is_file() && file.len() == metadata.size => "available",
                Ok(_) => "missing",
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing",
                Err(error) => {
                    tracing::warn!(%error, "activity attachment is unreadable");
                    "missing"
                }
            }
        };
        Ok(Attachment {
            metadata,
            status: status.into(),
            expires_at_ms,
        })
    }
    fn available(&self, metadata: AttachmentMetadata) -> Result<(Attachment, File)> {
        let attachment = self.describe(metadata, self.retention_days()?, now_ms())?;
        if attachment.status != "available" {
            bail!(ErrorCode::FileNotFound.error(format!("attachment is {}", attachment.status)));
        }
        let file = File::open(self.path(&attachment.metadata.id)?)?;
        Ok((attachment, file))
    }
    pub(crate) fn preview(&self, metadata: AttachmentMetadata) -> Result<AttachmentPreview> {
        let attachment = self.describe(metadata, self.retention_days()?, now_ms())?;
        let mut preview = AttachmentPreview {
            attachment,
            image: None,
            text: None,
        };
        if preview.attachment.status != "available"
            || preview.attachment.metadata.size > PREVIEW_LIMIT
        {
            return Ok(preview);
        }
        let (_, file) = self.available(preview.attachment.metadata.clone())?;
        let mut bytes = Vec::new();
        file.take(PREVIEW_LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != preview.attachment.metadata.size {
            bail!(ErrorCode::StorageError.error("attachment size changed"));
        }
        let mime = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some("image/png")
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some("image/jpeg")
        } else {
            None
        };
        if let Some(mime) = mime {
            preview.image = Some(format!("data:{mime};base64,{}", STANDARD.encode(&bytes)));
        } else if bytes.len() as u64 <= TEXT_LIMIT
            && let Ok(text) = std::str::from_utf8(&bytes)
            && !text
                .chars()
                .any(|ch| ch.is_control() && !['\n', '\r', '\t'].contains(&ch))
        {
            preview.text = Some(text.to_owned());
        }
        Ok(preview)
    }
    pub(crate) fn export(
        &self,
        metadata: AttachmentMetadata,
        destination: &Path,
    ) -> Result<PathBuf> {
        let (_, mut file) = self.available(metadata)?;
        crate::transfer::save_local_reader(destination, &mut file)
    }
    pub(crate) fn prune(&self) -> Result<()> {
        let days = self.retention_days()?;
        if days == 0 || !self.database.try_exists()? {
            return Ok(());
        }
        let db = rusqlite::Connection::open_with_flags(
            &self.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        db.busy_timeout(std::time::Duration::from_millis(500))?;
        let now = now_ms();
        let ids:Vec<String>=db.prepare("SELECT attachment_id FROM job_attachments WHERE status='available' AND created_at_ms<=?1")?
            .query_map([now.saturating_sub(i64::from(days)*DAY_MS)],|row|row.get(0))?.collect::<rusqlite::Result<_>>()?;
        for id in ids {
            match std::fs::remove_file(self.path(&id)?) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
            db.execute(
                "UPDATE job_attachments SET status='expired',deleted_at_ms=?2
                 WHERE attachment_id=?1 AND status='available'",
                rusqlite::params![id, now],
            )?;
        }
        Ok(())
    }
    // Called only at daemon startup, before accepting operations, so an in-flight
    // snapshot cannot be mistaken for an orphan between fsync and its DB insert.
    pub(crate) fn prune_orphans(&self) -> Result<()> {
        if !self.database.try_exists()? {
            return Ok(());
        }
        let db = rusqlite::Connection::open_with_flags(
            &self.database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )?;
        db.busy_timeout(std::time::Duration::from_millis(500))?;
        self.check_directory()?;
        let metadata = db.prepare(
            "SELECT attachment_id,name,size_bytes,sha256,created_at_ms FROM job_attachments WHERE status='available'"
        )?.query_map([], |row| read_metadata(row,0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for metadata in metadata {
            if self.describe(metadata.clone(), 0, now_ms())?.status == "missing" {
                db.execute(
                    "UPDATE job_attachments SET status='missing' WHERE attachment_id=?1",
                    [&metadata.id],
                )?;
            }
        }
        let known: std::collections::HashSet<String> = db
            .prepare("SELECT attachment_id FROM job_attachments")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|name| name.strip_suffix(".blob")) else {
                continue;
            };
            if created_at(id).is_some() && !known.contains(id) && entry.file_type()?.is_file() {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }
    pub(crate) fn prune_if_due(&self) {
        static LAST: OnceLock<Mutex<HashMap<PathBuf, i64>>> = OnceLock::new();
        let mut last = LAST.get_or_init(Mutex::default).lock().unwrap();
        let now = now_ms();
        if last
            .get(&self.dir)
            .is_some_and(|at| now - at < CLEANUP_INTERVAL_MS)
        {
            return;
        }
        match self.prune() {
            Ok(()) => {
                last.insert(self.dir.clone(), now);
            }
            Err(error) => tracing::warn!(%error, "activity attachment cleanup failed"),
        }
    }
}

fn created_at(id: &str) -> Option<i64> {
    let (time, uuid) = id.split_once('_')?;
    if time.len() > 19
        || time.is_empty()
        || !time.bytes().all(|ch| ch.is_ascii_digit())
        || uuid.len() != 32
        || !uuid
            .bytes()
            .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
    {
        return None;
    }
    time.parse().ok()
}

pub(crate) fn read_metadata(
    row: &rusqlite::Row<'_>,
    start: usize,
) -> rusqlite::Result<AttachmentMetadata> {
    Ok(AttachmentMetadata {
        id: row.get(start)?,
        name: row.get(start + 1)?,
        size: u64::try_from(row.get::<_, i64>(start + 2)?).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                start + 2,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        sha256: row.get(start + 3)?,
        created_at_ms: row.get(start + 4)?,
    })
}
pub(crate) fn for_job(
    db: &rusqlite::Connection,
    cache: &Cache,
    id: &str,
) -> Result<Vec<Attachment>> {
    let days = cache.retention_days()?;
    let now = now_ms();
    db.prepare(
        "SELECT attachment_id,name,size_bytes,sha256,created_at_ms,status FROM job_attachments
         WHERE job_id=?1 ORDER BY created_at_ms,attachment_id",
    )?
    .query_map([id], |row| {
        Ok((read_metadata(row, 0)?, row.get::<_, String>(5)?))
    })?
    .map(|row| {
        let (metadata, status) = row?;
        let mut attachment = cache.describe(metadata, days, now)?;
        if status != "available" {
            attachment.status = status;
        }
        Ok(attachment)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{protocol::*, store::JobStore};

    #[test]
    fn large_files_are_retained_and_exported_with_bounded_previews() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = JobStore::open(&dir.path().join("daemon.db"), true)?;
        let cache = store.attachments();
        let size = 64 * 1024 * 1024 + 1;
        let metadata = cache.save(std::io::repeat(0xa5).take(size), "large.bin")?;
        assert_eq!(metadata.size, size);
        let preview = cache.preview(metadata.clone())?;
        assert_eq!(preview.attachment.status, "available");
        assert!(preview.image.is_none());
        assert!(preview.text.is_none());
        let destination = dir.path().join("export.bin");
        cache.export(metadata.clone(), &destination)?;
        assert_eq!(std::fs::metadata(&destination)?.len(), size);
        assert_eq!(sha256(&std::fs::read(destination)?), metadata.sha256);
        Ok(())
    }

    #[tokio::test]
    async fn startup_cleans_only_orphan_snapshots_and_records_missing_copies() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.db");
        let store = JobStore::open(&path, true)?;
        let job = Job::accepted(
            "source",
            "target",
            &JobContext::new(&store.db_id),
            "hash".into(),
            JobDetails::Screenshot(ScreenshotParams {}),
        );
        store.insert(&job)?;
        let cache = store.attachments();
        let kept = cache.save(std::io::Cursor::new(b"referenced"), "kept.txt")?;
        store.attach(&job.job_id, kept.clone()).await?;
        let orphan = cache.save(std::io::Cursor::new(b"unrecorded"), "orphan.txt")?;
        let unrelated = cache.dir.join("unrelated.txt");
        std::fs::write(&unrelated, b"keep")?;
        cache.prune_orphans()?;
        assert_eq!(std::fs::read(cache.path(&kept.id)?)?, b"referenced");
        assert!(!cache.path(&orphan.id)?.exists());
        assert_eq!(std::fs::read(&unrelated)?, b"keep");
        std::fs::remove_file(cache.path(&kept.id)?)?;
        cache.prune_orphans()?;
        let db = rusqlite::Connection::open(&path)?;
        assert_eq!(
            db.query_row(
                "SELECT status FROM job_attachments WHERE attachment_id=?1",
                [&kept.id],
                |row| row.get::<_, String>(0)
            )?,
            "missing"
        );
        assert_eq!(
            store.get(&job.job_id)?.unwrap().attachments[0].status,
            "missing"
        );
        Ok(())
    }
}
