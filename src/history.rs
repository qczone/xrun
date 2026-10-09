//! Local activity summaries, attachments and read-only task output.
#![deny(missing_docs)]
use crate::error::ErrorCode;
use crate::{config, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};
mod activity;
pub use activity::{ActivityPage, activity, attachment, job, save_attachment};

const HISTORY_BUSY_TIMEOUT: Duration = Duration::from_millis(500);
const PAGE_SIZE: usize = 50;
const LOG_PAGE_SIZE: usize = 32;

#[derive(Debug, Serialize)]
/// Task snapshot and one page of sequenced output from the same read transaction.
pub struct JobOutput {
    /// Task snapshot associated with the response.
    pub job: Job,
    /// Sequenced output events, possibly followed by a newer task snapshot.
    pub events: Vec<LogEvent>,
    /// Whether another forward output page is available.
    pub has_more: bool,
}

fn open(path: &Path) -> Result<Option<Connection>> {
    if !path.try_exists()? {
        return Ok(None);
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(HISTORY_BUSY_TIMEOUT)?;
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != crate::store::JOB_SCHEMA_VERSION {
        bail!(ErrorCode::DbSchemaMismatch.error("unsupported task database schema"));
    }
    Ok(Some(db))
}

fn db_id(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT value FROM meta WHERE key='db_id'", [], |r| r.get(0))?)
}

/// Read local output and task status atomically. expected_db must match the task
/// list generation or DB_RESET is returned. None requests the newest tail; a sequence
/// requests forward paging. Missing tasks/databases fail, and records are never created.
pub fn output(expected_db: &str, job: &str, after: Option<u64>) -> Result<JobOutput> {
    output_at(
        &config::device_dir()?.join("daemon.db"),
        expected_db,
        job,
        after,
    )
}

fn output_at(path: &Path, expected_db: &str, job: &str, after: Option<u64>) -> Result<JobOutput> {
    let mut db =
        open(path)?.context(ErrorCode::DbMissing.error("task database is no longer available"))?;
    let tx = db.transaction()?;
    if db_id(&tx)? != expected_db {
        bail!(ErrorCode::DbReset.error("task database has been replaced; refresh the task list"));
    }
    let mut job = tx
        .query_row(
            &format!(
                "SELECT {} FROM jobs WHERE job_id=?1",
                crate::store::JOB_COLUMNS
            ),
            [job],
            |row| crate::store::read_job(row, 0),
        )
        .optional()?
        .context(ErrorCode::JobNotFound.error("task is no longer available"))?;
    if job.kind() != JobKind::Exec {
        bail!(ErrorCode::LogUnavailable.error("this job does not retain command output"));
    }
    job.attachments = crate::attachments::for_job(
        &tx,
        &crate::attachments::Cache::for_database(path),
        &job.job_id,
    )?;
    let after = after
        .map(i64::try_from)
        .transpose()
        .context(ErrorCode::InvalidCursor.error("log sequence is too large"))?;
    let sql = if after.is_some() {
        "SELECT seq,stream,bytes FROM job_logs WHERE job_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3"
    } else {
        "SELECT seq,stream,bytes FROM job_logs WHERE job_id=?1 AND seq>?2 ORDER BY seq DESC LIMIT ?3"
    };
    let mut stmt = tx.prepare(sql)?;
    let mut events = stmt
        .query_map(
            params![job.job_id, after.unwrap_or(0), (LOG_PAGE_SIZE + 1) as i64],
            |r| {
                Ok(LogEvent {
                    seq: r.get::<_, i64>(0)? as u64,
                    stream: r.get(1)?,
                    data_base64: STANDARD.encode(r.get::<_, Vec<u8>>(2)?),
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = events.len() > LOG_PAGE_SIZE;
    events.truncate(LOG_PAGE_SIZE);
    if after.is_none() {
        events.reverse();
    }
    Ok(JobOutput {
        job,
        events,
        has_more: after.is_some() && more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::JobStore;
    fn command(id: usize, db: &str) -> Job {
        let mut job = Job::accepted(
            "source",
            "target",
            &JobContext {
                request_id: format!("request-{id}"),
                db_id: db.into(),
            },
            "hash".into(),
            JobDetails::Exec(CommandParams {
                program: "echo".into(),
                args: vec!["hello".into()],
                cwd: "/tmp".into(),
                timeout: 0,
                shell: None,
                input_size: None,
                input_sha256: None,
            }),
        );
        job.job_id = format!("{id:06}");
        job.state = JobState::Running;
        job
    }
    fn operation(id: usize, db: &str, details: JobDetails, state: JobState, time: i64) -> Job {
        let mut job = Job::accepted(
            "source",
            "target",
            &JobContext {
                request_id: format!("operation-{id}"),
                db_id: db.into(),
            },
            "hash".into(),
            details,
        );
        job.job_id = format!("P{id:05}");
        job.state = state;
        job.created_at_ms = time;
        job
    }
    #[test]
    fn output_remains_read_only_and_pages_binary_logs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.db");
        assert!(
            activity::activity_at(&path, None, "all")?
                .entries
                .is_empty()
        );
        assert!(!path.exists());
        let store = JobStore::open(&path, true)?;
        store.insert(&command(1, &store.db_id))?;
        assert!(
            open(&path)?
                .unwrap()
                .execute("DELETE FROM jobs", [])
                .is_err()
        );
        assert!(crate::error::is(
            &output_at(&path, "old-db", "000001", None).unwrap_err(),
            ErrorCode::DbReset
        ));
        for _ in 0..40 {
            store.append("000001", "stdout", b"hello\n")?;
        }
        let tail = output_at(&path, &store.db_id, "000001", None)?;
        assert_eq!(tail.events.first().unwrap().seq, 9);
        assert_eq!(tail.events.last().unwrap().seq, 40);
        store.append("000001", "stderr", &[0xff, 0, 0xe4, 0xb8, 0xad])?;
        let next = output_at(&path, &store.db_id, "000001", Some(40))?;
        assert_eq!(next.events.len(), 1);
        assert_eq!(
            STANDARD.decode(&next.events[0].data_base64)?,
            [0xff, 0, 0xe4, 0xb8, 0xad]
        );
        Ok(())
    }
    #[test]
    fn journey_reads_one_job_table_and_filters_every_operation() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.db");
        let store = JobStore::open(&path, true)?;
        let mut job = command(1, &store.db_id);
        job.created_at_ms = 300;
        store.insert(&job)?;
        for (id, time, state, details) in [
            (
                1,
                400,
                JobState::Running,
                JobDetails::Push(PushParams {
                    path: "x".into(),
                    cwd: None,
                    size: 0,
                    sha256: sha256(b""),
                    mkdir: false,
                    no_overwrite: false,
                    expect: None,
                }),
            ),
            (
                2,
                200,
                JobState::Succeeded,
                JobDetails::Screenshot(ScreenshotParams {}),
            ),
            (
                3,
                100,
                JobState::Lost,
                JobDetails::Forward(ForwardParams { port: 3000 }),
            ),
            (
                4,
                250,
                JobState::Failed,
                JobDetails::StreamExec(job.details.command().unwrap().clone()),
            ),
        ] {
            store.insert(&operation(id, &store.db_id, details, state, time))?;
        }
        let page = activity::activity_at(&path, None, "all")?;
        assert_eq!(
            page.entries
                .iter()
                .map(|job| job.created_at_ms)
                .collect::<Vec<_>>(),
            [400, 300, 250, 200, 100]
        );
        assert_eq!(page.entries[0].kind(), JobKind::Push);
        assert_eq!(
            activity::activity_at(&path, None, "failed")?.entries.len(),
            2
        );
        assert_eq!(
            activity::activity_at(&path, None, "running")?.entries.len(),
            2
        );
        assert!(crate::error::is(
            &activity::activity_at(&path, None, "bad").unwrap_err(),
            ErrorCode::InvalidFilter
        ));
        assert!(crate::error::is(
            &output_at(&path, &store.db_id, "P00001", None).unwrap_err(),
            ErrorCode::LogUnavailable
        ));
        let detail = activity::job_at(&path, &store.db_id, "P00001")?;
        assert_eq!(detail.kind(), JobKind::Push);
        Ok(())
    }
    #[test]
    fn journey_cursor_survives_equal_times_updates_and_new_insertions() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.db");
        let store = JobStore::open(&path, true)?;
        for n in 1..=55 {
            let mut job = command(n, &store.db_id);
            job.created_at_ms = 1000;
            store.insert(&job)?;
            store.insert(&operation(
                n,
                &store.db_id,
                JobDetails::Pull(PullParams {
                    path: "x".into(),
                    cwd: None,
                }),
                JobState::Succeeded,
                1000,
            ))?;
        }
        let first = activity::activity_at(&path, None, "all")?;
        assert_eq!(first.entries.len(), 50);
        let cursor = first.next_cursor.clone().unwrap();
        let mut newer = command(56, &store.db_id);
        newer.created_at_ms = 2000;
        store.insert(&newer)?;
        store.finish_sync("000001", crate::store::JobOutcome::new(JobState::Failed))?;
        let mut entries = first.entries;
        let mut before = Some(cursor.clone());
        while let Some(cursor) = before {
            let page = activity::activity_at(&path, Some(&cursor), "all")?;
            entries.extend(page.entries);
            before = page.next_cursor;
        }
        assert_eq!(entries.len(), 110);
        assert_eq!(
            entries
                .iter()
                .map(|job| &job.job_id)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            110
        );
        assert!(crate::error::is(
            &activity::activity_at(&path, Some("invalid"), "all").unwrap_err(),
            ErrorCode::InvalidCursor
        ));
        Connection::open(&path)?
            .execute("UPDATE meta SET value='replaced' WHERE key='db_id'", [])?;
        assert!(crate::error::is(
            &activity::activity_at(&path, Some(&cursor), "all").unwrap_err(),
            ErrorCode::DbReset
        ));
        assert!(crate::error::is(
            &activity::job_at(&path, &store.db_id, "000001").unwrap_err(),
            ErrorCode::DbReset
        ));
        Ok(())
    }
    #[tokio::test]
    async fn attachments_are_independent_and_expiry_preserves_job_and_metadata() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.db");
        let store = JobStore::open(&path, true)?;
        let job = operation(
            1,
            &store.db_id,
            JobDetails::Pull(PullParams {
                path: "original.txt".into(),
                cwd: None,
            }),
            JobState::Succeeded,
            1000,
        );
        store.insert(&job)?;
        let original = dir.path().join("original.txt");
        std::fs::write(&original, b"original bytes")?;
        let cache = store.attachments();
        let mut metadata = cache.save_file(&original, "original.txt")?;
        std::fs::write(&original, b"changed original")?;
        assert_eq!(
            cache.preview(metadata.clone())?.text.as_deref(),
            Some("original bytes")
        );
        let export = dir.path().join("copy.txt");
        cache.export(metadata.clone(), &export)?;
        assert_eq!(std::fs::read(&export)?, b"original bytes");
        let root = dir.path().join("attachments");
        let old = root.join(format!("{}.blob", metadata.id));
        metadata.id = "1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
        metadata.created_at_ms = 1;
        std::fs::rename(old, root.join(format!("{}.blob", metadata.id)))?;
        store.attach(&job.job_id, metadata.clone()).await?;
        cache.prune()?;
        let page = activity::activity_at(&path, None, "all")?;
        assert_eq!(page.entries.len(), 1);
        let attachment = &page.entries[0].attachments[0];
        assert_eq!(attachment.status, "expired");
        assert_eq!(attachment.metadata.name, "original.txt");
        assert!(!root.join(format!("{}.blob", metadata.id)).exists());
        assert_eq!(std::fs::read(&original)?, b"changed original");
        assert!(
            cache
                .export(metadata, &dir.path().join("expired.txt"))
                .is_err()
        );
        assert_eq!(Connection::open(&path)?.query_row(
            "SELECT COUNT(*) FROM job_attachments WHERE status='expired' AND deleted_at_ms IS NOT NULL",
            [], |row| row.get::<_,i64>(0)
        )?,1);
        Ok(())
    }
}
