pub(crate) mod jobs;
mod logs;
mod submissions;
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
    crate::database::configure(&db)?;
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

pub(crate) use jobs::JobOutcome;
pub(crate) use jobs::{JOB_COLUMNS, JOB_SCHEMA_VERSION, merge_incomplete, read_job};

pub struct JobStore {
    worker: worker::Worker,
    attachments: crate::attachments::Cache,
    pub db_id: String,
    changes: tokio::sync::watch::Sender<()>,
}
impl JobStore {
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        let database = jobs::Database::open(path, create)?;
        Ok(Self {
            attachments: crate::attachments::Cache::for_database(path),
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
    pub(crate) fn unfinished(&self) -> Result<Vec<Job>> {
        self.worker.call(|db| db.unfinished())
    }
    pub(crate) async fn unfinished_ids(&self) -> Result<Vec<String>> {
        self.worker.query(|db| db.unfinished_ids()).await
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
    pub fn insert(&self, job: &Job) -> Result<()> {
        let job = job.clone();
        self.worker.call(move |db| db.insert(&job))
    }
    pub(crate) async fn page_bounded_async(
        &self,
        source: &str,
        running: bool,
        request: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Job>, Option<usize>)> {
        let source = source.to_owned();
        let request = request.map(str::to_owned);
        self.worker
            .query(move |db| db.page_bounded(&source, running, request.as_deref(), limit, offset))
            .await
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
    pub(crate) async fn tail_async(
        &self,
        id: &str,
        after: u64,
        through: u64,
        lines: usize,
    ) -> Result<Vec<LogEvent>> {
        let id = id.to_owned();
        self.worker
            .query(move |db| db.tail(&id, after, through, lines))
            .await
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
        self.worker.call(|db| db.prune())?;
        self.attachments.prune_if_due();
        Ok(())
    }
    pub(crate) fn attachments(&self) -> crate::attachments::Cache {
        self.attachments.clone()
    }
    pub(crate) async fn attach(
        &self,
        id: &str,
        metadata: crate::attachments::AttachmentMetadata,
    ) -> Result<()> {
        let id = id.to_owned();
        self.worker.query(move |db| db.attach(&id, metadata)).await
    }
    pub(crate) fn finish_detached(&self, id: String, outcome: JobOutcome) -> Result<()> {
        self.worker.enqueue(Box::new(move |db| {
            if let Err(error) = db.finish(&id, outcome) {
                tracing::error!(%error,"interrupted job could not be finalized");
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
        self.finish(id, JobOutcome::failed(&anyhow::anyhow!(error)))
            .await
    }
    /// Waits for operations already queued, including work whose reader was canceled.
    pub(crate) async fn flush(&self) -> Result<()> {
        self.worker.query(|_| Ok(())).await
    }
}
