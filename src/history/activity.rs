//! Chronological views of the single authoritative job table.
use super::*;
use crate::attachments::{AttachmentMetadata, AttachmentPreview, Cache};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

#[derive(Debug, Serialize)]
/// One page of target-local jobs, newest accepted operation first.
pub struct ActivityPage {
    /// Database generation, shared by details and output reads.
    pub db_id: Option<String>,
    /// All business operations in one model.
    pub entries: Vec<Job>,
    /// Opaque cursor passed back unchanged.
    pub next_cursor: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    db_id: String,
    time: i64,
    job_id: String,
}
/// Read locally accepted jobs without contacting other devices.
pub fn activity(before: Option<&str>, filter: &str) -> Result<ActivityPage> {
    activity_at(&config::device_dir()?.join("daemon.db"), before, filter)
}
pub(super) fn activity_at(path: &Path, before: Option<&str>, filter: &str) -> Result<ActivityPage> {
    let selected = match filter {
        "all" => "1",
        "running" => "state IN ('accepted','running')",
        "failed" => "state IN ('failed','canceled','timed_out','lost')",
        _ => bail!(ErrorCode::InvalidFilter.error("expected all, running or failed")),
    };
    let cursor = before
        .map(|value| -> Result<Cursor> {
            if value.len() > 2048 {
                bail!("cursor is too long");
            }
            let cursor: Cursor = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(value)?)?;
            if cursor.job_id.is_empty() || cursor.job_id.len() > 128 || cursor.time < 0 {
                bail!("invalid cursor position");
            }
            Ok(cursor)
        })
        .transpose()
        .map_err(|error| anyhow::anyhow!(ErrorCode::InvalidCursor.error(error.to_string())))?;
    let Some(mut db) = open(path)? else {
        if cursor.is_some() {
            bail!(ErrorCode::DbMissing.error("activity database is no longer available"));
        }
        return Ok(ActivityPage {
            db_id: None,
            entries: vec![],
            next_cursor: None,
        });
    };
    let cache = Cache::for_database(path);
    cache.prune_if_due();
    let tx = db.transaction()?;
    let generation = db_id(&tx)?;
    if cursor
        .as_ref()
        .is_some_and(|cursor| cursor.db_id != generation)
    {
        bail!(
            ErrorCode::DbReset
                .error("activity database has been replaced; return to the newest page")
        );
    }
    let sql = format!(
        "SELECT {} FROM jobs WHERE {selected} AND (?1 IS NULL OR (created_at_ms,job_id)<(?1,?2))
        ORDER BY created_at_ms DESC,job_id DESC LIMIT ?3",
        crate::store::JOB_COLUMNS
    );
    let mut statement = tx.prepare(&sql)?;
    let mut rows = statement.query(params![
        cursor.as_ref().map(|cursor| cursor.time),
        cursor.as_ref().map(|cursor| &cursor.job_id),
        (PAGE_SIZE + 1) as i64
    ])?;
    let mut entries = vec![];
    let mut more = false;
    while let Some(row) = rows.next()? {
        if entries.len() == PAGE_SIZE {
            more = true;
            break;
        }
        let mut job = crate::store::read_job(row, 0)?;
        job.attachments = crate::attachments::for_job(&tx, &cache, &job.job_id)?;
        entries.push(job);
    }
    let next_cursor = if more {
        entries
            .last()
            .map(|job| {
                serde_json::to_vec(&Cursor {
                    db_id: generation.clone(),
                    time: job.created_at_ms,
                    job_id: job.job_id.clone(),
                })
                .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
            })
            .transpose()?
    } else {
        None
    };
    Ok(ActivityPage {
        db_id: Some(generation),
        entries,
        next_cursor,
    })
}
/// Read a job snapshot and its attachments within the expected database generation.
pub fn job(expected_db: &str, id: &str) -> Result<Job> {
    job_at(&config::device_dir()?.join("daemon.db"), expected_db, id)
}
pub(super) fn job_at(path: &Path, expected_db: &str, id: &str) -> Result<Job> {
    let Some(mut db) = open(path)? else {
        bail!(ErrorCode::DbMissing.error("activity database is no longer available"));
    };
    let tx = db.transaction()?;
    if db_id(&tx)? != expected_db {
        bail!(ErrorCode::DbReset.error("activity database has been replaced"));
    }
    let mut job = crate::store::jobs::job_at(&tx, id)?
        .context(ErrorCode::JobNotFound.error("unknown local job"))?;
    job.attachments = crate::attachments::for_job(&tx, &Cache::for_database(path), id)?;
    Ok(job)
}
pub(super) fn attachment_at(path: &Path, id: &str) -> Result<(Cache, AttachmentMetadata, String)> {
    let db = open(path)?
        .context(ErrorCode::DbMissing.error("activity database is no longer available"))?;
    let (metadata,status)=db.query_row("SELECT attachment_id,name,size_bytes,sha256,created_at_ms,status FROM job_attachments WHERE attachment_id=?1",
        [id],|row|Ok((crate::attachments::read_metadata(row,0)?,row.get::<_,String>(5)?))).optional()?
        .context(ErrorCode::FileNotFound.error("attachment is not recorded in local activity"))?;
    Ok((Cache::for_database(path), metadata, status))
}
/// Preview a recorded copy without reading its original source path.
pub fn attachment(id: &str) -> Result<AttachmentPreview> {
    let (cache, metadata, status) = attachment_at(&config::device_dir()?.join("daemon.db"), id)?;
    let preview = if status == "available" {
        cache.preview(metadata)?
    } else {
        let mut attachment = cache.describe(metadata, cache.retention_days()?, now_ms())?;
        attachment.status = status;
        AttachmentPreview {
            attachment,
            image: None,
            text: None,
        }
    };
    Ok(preview)
}
/// Export a recorded, currently available copy to a chosen destination.
pub fn save_attachment(id: &str, destination: &Path) -> Result<std::path::PathBuf> {
    let (cache, metadata, status) = attachment_at(&config::device_dir()?.join("daemon.db"), id)?;
    if status != "available" {
        bail!(ErrorCode::FileNotFound.error(format!("attachment is {status}")));
    }
    cache.export(metadata, destination)
}
