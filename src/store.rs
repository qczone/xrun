mod logs;
mod submissions;
mod tasks;
mod worker;
use crate::error::ErrorCode;
use crate::{config::restrict_dir, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Mutex};
pub(crate) use submissions::{Submission, SubmissionStore};

const MAX_JOB_LOG_BYTES: i64 = 64 * 1024 * 1024;
const MAX_TOTAL_LOG_BYTES: i64 = 1024 * 1024 * 1024;
const FINISHED_LOG_RETENTION_MS: i64 = 7 * 86_400_000;
const AUDIT_RETENTION_MS: i64 = 7 * 86_400_000;
const SUBMISSION_RETENTION_MS: i64 = 7 * 86_400_000;

fn open(path: &Path, create: bool) -> Result<Connection> {
    if !create && !path.exists() {
        bail!(ErrorCode::DbMissing.error(format!(
            "{} (use daemon reset while stopped)",
            path.display()
        )))
    }
    let parent = path.parent().context("missing database parent")?;
    std::fs::create_dir_all(parent)?;
    restrict_dir(parent)?;
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "FULL")?;
    let integrity: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        bail!(ErrorCode::DbCorrupt.error(integrity.to_string()))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(db)
}
fn decode<T: serde::de::DeserializeOwned>(s: String) -> Result<T> {
    Ok(serde_json::from_str(&s)?)
}

pub(crate) use tasks::JobOutcome;
pub(crate) use tasks::{JOB_COLUMNS, TASK_SCHEMA_VERSION, merge_incomplete, read_job};

pub struct TaskStore {
    worker: worker::Worker,
    pub db_id: String,
    changes: tokio::sync::watch::Sender<()>,
}
impl TaskStore {
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        let database = tasks::Database::open(path, create)?;
        Ok(Self {
            db_id: database.db_id.clone(),
            changes: database.changes.clone(),
            worker: worker::Worker::start(database)?,
        })
    }
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes.subscribe()
    }
    pub fn get(&self, id: &str) -> Result<Option<Job>> {
        let id = id.to_owned();
        self.worker.call(move |db| db.get(&id))
    }
    pub(crate) async fn get_async(&self, id: &str) -> Result<Option<Job>> {
        let id = id.to_owned();
        self.worker.query(move |db| db.get(&id)).await
    }
    pub fn by_request(&self, source: &str, id: &str) -> Result<Option<Job>> {
        let source = source.to_owned();
        let id = id.to_owned();
        self.worker.call(move |db| db.by_request(&source, &id))
    }
    pub fn all(&self) -> Result<Vec<Job>> {
        self.worker.call(|db| db.all())
    }
    pub(crate) async fn all_async(&self) -> Result<Vec<Job>> {
        self.worker.query(|db| db.all()).await
    }
    pub fn active_count(&self) -> Result<usize> {
        self.worker.call(|db| db.active_count())
    }
    pub fn page(
        &self,
        source: &str,
        running: bool,
        request: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Job>> {
        let source = source.to_owned();
        let request = request.map(str::to_owned);
        self.worker
            .call(move |db| db.page(&source, running, request.as_deref(), limit, offset))
    }
    pub(crate) async fn page_async(
        &self,
        source: &str,
        running: bool,
        request: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Job>> {
        let source = source.to_owned();
        let request = request.map(str::to_owned);
        self.worker
            .query(move |db| db.page(&source, running, request.as_deref(), limit, offset))
            .await
    }
    pub fn insert(&self, job: &Job) -> Result<()> {
        let job = job.clone();
        self.worker.call(move |db| db.insert(&job))
    }
    pub(crate) fn replace_fixture(&self, job: &Job) -> Result<()> {
        let job = job.clone();
        self.worker.call(move |db| db.replace_fixture(&job))
    }
    pub fn logs(&self, id: &str, after: u64) -> Result<Vec<LogEvent>> {
        let id = id.to_owned();
        self.worker.call(move |db| db.logs(&id, after))
    }
    pub(crate) async fn logs_async(&self, id: &str, after: u64) -> Result<Vec<LogEvent>> {
        let id = id.to_owned();
        self.worker.query(move |db| db.logs(&id, after)).await
    }
    pub fn append(&self, id: &str, stream: &str, bytes: &[u8]) -> Result<Option<u64>> {
        let id = id.to_owned();
        let stream = stream.to_owned();
        let bytes = bytes.to_vec();
        self.worker.call(move |db| db.append(&id, &stream, &bytes))
    }
    pub(crate) async fn append_async(
        &self,
        id: &str,
        stream: &str,
        bytes: Vec<u8>,
    ) -> Result<Option<u64>> {
        let id = id.to_owned();
        let stream = stream.to_owned();
        self.worker
            .query(move |db| db.append(&id, &stream, &bytes))
            .await
    }
    pub fn prune(&self) -> Result<()> {
        self.worker.call(|db| db.prune())
    }
    pub fn audit(&self, value: serde_json::Value) -> Result<()> {
        self.worker.call(move |db| db.audit(value))
    }
    pub(crate) async fn audit_async(&self, value: serde_json::Value) -> Result<()> {
        self.worker.query(move |db| db.audit(value)).await
    }
    pub(crate) fn audit_detached(&self, value: serde_json::Value) -> Result<()> {
        self.worker.enqueue(Box::new(move |db| {
            if let Err(error) = db.audit(value) {
                tracing::error!(%error, "operation audit could not be saved");
            }
        }))
    }
    pub(crate) fn record_process(&self, id: &str, process: ProcessIdentity) -> Result<Job> {
        let id = id.to_owned();
        self.worker.call(move |db| db.record_process(&id, process))
    }
    pub(crate) async fn mark_running(&self, id: &str) -> Result<Job> {
        let id = id.to_owned();
        self.worker.query(move |db| db.mark_running(&id)).await
    }
    pub(crate) fn finish_sync(&self, id: &str, outcome: JobOutcome) -> Result<Job> {
        let id = id.to_owned();
        self.worker.call(move |db| db.finish(&id, outcome))
    }
    pub(crate) async fn finish(&self, id: &str, outcome: JobOutcome) -> Result<Job> {
        let id = id.to_owned();
        self.worker.query(move |db| db.finish(&id, outcome)).await
    }
    pub(crate) async fn mark_failed(&self, id: &str, error: String) -> Result<Job> {
        self.finish(
            id,
            JobOutcome {
                state: JobState::Failed,
                exit_code: None,
                signal: None,
                duration_ms: None,
                error: Some(error),
                incomplete_reason: None,
                leftover_possible: false,
            },
        )
        .await
    }
    /// Waits for operations already queued, including work whose reader was canceled.
    pub(crate) async fn flush(&self) -> Result<()> {
        self.worker.query(|_| Ok(())).await
    }
}
