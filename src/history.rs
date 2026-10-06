//! Local, read-only views of the daemon's existing task and file records.
use crate::error::ErrorCode;
use crate::{config, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{path::Path, time::Duration};

const PAGE_SIZE: usize = 50;
const LOG_PAGE_SIZE: usize = 32;

#[derive(Serialize)]
pub struct TaskPage {
    pub db_id: Option<String>,
    pub jobs: Vec<Job>,
    pub next_cursor: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct TaskOutput {
    pub job: Job,
    pub events: Vec<LogEvent>,
    pub has_more: bool,
}

#[derive(Deserialize, Serialize)]
pub struct FileRecord {
    #[serde(default)]
    pub time_ms: i64,
    pub source_device_id: String,
    pub op: String,
    pub path: Option<String>,
    pub size: Option<u64>,
    pub result: String,
}

#[derive(Serialize)]
pub struct FilePage {
    pub entries: Vec<FileRecord>,
    pub next_cursor: Option<i64>,
}

fn open(path: &Path) -> Result<Option<Connection>> {
    if !path.try_exists()? {
        return Ok(None);
    }
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(Duration::from_millis(500))?;
    let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != crate::store::TASK_SCHEMA_VERSION {
        bail!(ErrorCode::DbSchemaMismatch.error("unsupported task database schema"));
    }
    Ok(Some(db))
}

fn db_id(db: &Connection) -> Result<String> {
    Ok(db.query_row("SELECT value FROM meta WHERE key='db_id'", [], |r| r.get(0))?)
}

pub fn tasks(before: Option<i64>, filter: &str) -> Result<TaskPage> {
    tasks_at(&config::device_dir()?.join("daemon.db"), before, filter)
}

fn tasks_at(path: &Path, before: Option<i64>, filter: &str) -> Result<TaskPage> {
    if !["all", "running", "failed"].contains(&filter) {
        bail!(ErrorCode::InvalidFilter.error("expected all, running or failed"));
    }
    let Some(mut db) = open(path)? else {
        return Ok(TaskPage {
            db_id: None,
            jobs: vec![],
            next_cursor: None,
        });
    };
    let tx = db.transaction()?;
    let id = db_id(&tx)?;
    let mut stmt = tx.prepare(&format!(
        "SELECT rowid,{} FROM jobs WHERE (?1 IS NULL OR rowid<?1) AND (
            ?2='all' OR (?2='running' AND state IN ('starting','running')) OR
            (?2='failed' AND (state IN ('failed','canceled','timed_out','lost') OR
                (state='exited' AND (exit_code!=0 OR signal IS NOT NULL)))))
            ORDER BY rowid DESC LIMIT ?3",
        crate::store::JOB_COLUMNS
    ))?;
    let mut rows = stmt.query(params![before, filter, (PAGE_SIZE + 1) as i64])?;
    let mut jobs = Vec::new();
    let mut last_row = None;
    let mut more = false;
    while let Some(row) = rows.next()? {
        if jobs.len() == PAGE_SIZE {
            more = true;
            break;
        }
        last_row = Some(row.get(0)?);
        jobs.push(crate::store::read_job(row, 1)?);
    }
    Ok(TaskPage {
        db_id: Some(id),
        jobs,
        next_cursor: more.then_some(last_row).flatten(),
    })
}

pub fn output(expected_db: &str, job: &str, after: Option<u64>) -> Result<TaskOutput> {
    output_at(
        &config::device_dir()?.join("daemon.db"),
        expected_db,
        job,
        after,
    )
}

fn output_at(path: &Path, expected_db: &str, job: &str, after: Option<u64>) -> Result<TaskOutput> {
    let mut db =
        open(path)?.context(ErrorCode::DbMissing.error("task database is no longer available"))?;
    let tx = db.transaction()?;
    if db_id(&tx)? != expected_db {
        bail!(ErrorCode::DbReset.error("task database has been replaced; refresh the task list"));
    }
    let job = tx
        .query_row(
            &format!("SELECT {} FROM jobs WHERE id=?1", crate::store::JOB_COLUMNS),
            [job],
            |row| crate::store::read_job(row, 0),
        )
        .optional()?
        .context(ErrorCode::JobNotFound.error("task is no longer available"))?;
    let after = after
        .map(i64::try_from)
        .transpose()
        .context(ErrorCode::InvalidCursor.error("log sequence is too large"))?;
    let sql = if after.is_some() {
        "SELECT seq,stream,bytes FROM logs WHERE job=?1 AND seq>?2 ORDER BY seq LIMIT ?3"
    } else {
        "SELECT seq,stream,bytes FROM logs WHERE job=?1 AND seq>?2 ORDER BY seq DESC LIMIT ?3"
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
    Ok(TaskOutput {
        job,
        events,
        has_more: after.is_some() && more,
    })
}

pub fn files(before: Option<i64>) -> Result<FilePage> {
    files_at(&config::device_dir()?.join("daemon.db"), before)
}

fn files_at(path: &Path, before: Option<i64>) -> Result<FilePage> {
    let Some(db) = open(path)? else {
        return Ok(FilePage {
            entries: vec![],
            next_cursor: None,
        });
    };
    let mut stmt = db.prepare(
        "SELECT rowid,time,data FROM audit WHERE (?1 IS NULL OR rowid<?1) AND json_extract(data,'$.op') IN ('push','pull','screenshot') ORDER BY rowid DESC LIMIT ?2",
    )?;
    let mut rows = stmt.query(params![before, (PAGE_SIZE + 1) as i64])?;
    let mut entries = Vec::new();
    let mut last_row = None;
    let mut more = false;
    while let Some(row) = rows.next()? {
        if entries.len() == PAGE_SIZE {
            more = true;
            break;
        }
        last_row = Some(row.get(0)?);
        let mut record: FileRecord = serde_json::from_str(&row.get::<_, String>(2)?)?;
        record.time_ms = row.get(1)?;
        entries.push(record);
    }
    Ok(FilePage {
        entries,
        next_cursor: more.then_some(last_row).flatten(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::TaskStore;

    fn job(id: usize, db: &str) -> Job {
        Job {
            job_id: format!("{id:06}"),
            request_id: format!("request-{id}"),
            request_hash: String::new(),
            source_device_id: "source".into(),
            target_device_id: "target".into(),
            db_id: db.into(),
            program: "echo".into(),
            args: vec!["hello".into()],
            cwd: "/tmp".into(),
            state: JobState::Running,
            exit_code: None,
            signal: None,
            duration_ms: None,
            last_seq: 0,
            output_complete: true,
            incomplete_reason: None,
            error: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
            leftover_possible: false,
            process: None,
        }
    }

    #[test]
    fn history_is_read_only_and_missing_database_stays_missing() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.db");
        assert!(tasks_at(&path, None, "all")?.jobs.is_empty());
        assert!(files_at(&path, None)?.entries.is_empty());
        assert!(!path.exists());
        let store = TaskStore::open(&path, true)?;
        store.insert(&job(1, &store.db_id))?;
        let db = open(&path)?.unwrap();
        assert!(db.execute("DELETE FROM jobs", []).is_err());
        assert_eq!(tasks_at(&path, None, "all")?.jobs.len(), 1);
        assert!(crate::error::is(
            &output_at(&path, "old-db", "000001", None).unwrap_err(),
            ErrorCode::DbReset
        ));
        Ok(())
    }

    #[test]
    fn pages_survive_new_tasks_and_logs_follow_by_sequence() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("daemon.db");
        let store = TaskStore::open(&path, true)?;
        for id in 1..=55 {
            store.insert(&job(id, &store.db_id))?;
        }
        let page = tasks_at(&path, None, "running")?;
        assert_eq!(page.jobs.len(), 50);
        store.insert(&job(56, &store.db_id))?;
        let older = tasks_at(&path, page.next_cursor, "running")?;
        assert_eq!(older.jobs.len(), 5);
        assert_eq!(older.jobs[0].job_id, "000005");
        assert!(older.next_cursor.is_none());
        assert!(tasks_at(&path, None, "failed")?.jobs.is_empty());
        let mut failed = job(56, &store.db_id);
        failed.state = JobState::Exited;
        failed.exit_code = Some(1);
        store.replace_fixture(&failed)?;
        assert_eq!(tasks_at(&path, None, "failed")?.jobs[0].job_id, "000056");
        for _ in 0..40 {
            store.append("000055", "stdout", b"hello\n")?;
        }
        let tail = output_at(&path, &store.db_id, "000055", None)?;
        assert_eq!(tail.events.first().unwrap().seq, 9);
        assert_eq!(tail.events.last().unwrap().seq, 40);
        store.append("000055", "stderr", &[0xff, 0, 0xe4, 0xb8, 0xad])?;
        let next = output_at(&path, &store.db_id, "000055", Some(40))?;
        assert_eq!(next.events.len(), 1);
        assert_eq!(
            STANDARD.decode(&next.events[0].data_base64)?,
            [0xff, 0, 0xe4, 0xb8, 0xad]
        );
        store.audit(serde_json::json!({"source_device_id":"source", "op":"pull", "path":"file.txt", "size":6, "result":"ok"}))?;
        store.audit(serde_json::json!({"source_device_id":"source", "op":"forward", "port":3000, "result":"ok"}))?;
        assert_eq!(files_at(&path, None)?.entries[0].op, "pull");
        assert_eq!(files_at(&path, None)?.entries.len(), 1);
        Ok(())
    }
}
