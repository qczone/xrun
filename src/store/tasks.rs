//! Task rows and legal lifecycle updates. Hot fields have one storage owner.
use super::*;

pub(crate) const JOB_COLUMNS: &str =
    "data,state,last_seq,output_complete,incomplete_reason,updated_at_ms,exit_code,signal";
pub(crate) const TASK_SCHEMA_VERSION: i64 = 1;
const TASK_SCHEMA: &str = "
    CREATE TABLE meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);
    CREATE TABLE jobs(
        id TEXT PRIMARY KEY, source TEXT NOT NULL, request_id TEXT NOT NULL,
        data TEXT NOT NULL, state TEXT NOT NULL, last_seq INTEGER NOT NULL,
        output_complete INTEGER NOT NULL, incomplete_reason TEXT,
        updated_at_ms INTEGER NOT NULL, exit_code INTEGER, signal INTEGER,
        UNIQUE(source,request_id)
    );
    CREATE INDEX jobs_active ON jobs(source) WHERE state IN ('starting','running');
    CREATE INDEX jobs_source ON jobs(source);
    CREATE INDEX jobs_finished ON jobs(updated_at_ms) WHERE state NOT IN ('starting','running');
    CREATE TABLE logs(job TEXT NOT NULL,seq INTEGER NOT NULL,stream TEXT NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(job,seq));
    CREATE TABLE log_sizes(job TEXT PRIMARY KEY,bytes INTEGER NOT NULL);
    INSERT INTO meta VALUES('log_bytes','0');
    CREATE TABLE audit(time INTEGER NOT NULL,data TEXT NOT NULL);
";

pub(super) struct Database {
    pub(super) db: Connection,
    pub(super) db_id: String,
    pub(super) changes: tokio::sync::watch::Sender<()>,
}
pub(crate) fn read_job(row: &rusqlite::Row<'_>, start: usize) -> rusqlite::Result<Job> {
    let data: String = row.get(start)?;
    let decode = || -> Result<Job> {
        let mut value: serde_json::Value = serde_json::from_str(&data)?;
        let fields = value
            .as_object_mut()
            .context("task payload must be an object")?;
        fields.insert("state".into(), row.get::<_, String>(start + 1)?.into());
        fields.insert("last_seq".into(), row.get::<_, i64>(start + 2)?.into());
        fields.insert(
            "output_complete".into(),
            row.get::<_, bool>(start + 3)?.into(),
        );
        fields.insert(
            "incomplete_reason".into(),
            serde_json::to_value(row.get::<_, Option<String>>(start + 4)?)?,
        );
        fields.insert("updated_at_ms".into(), row.get::<_, i64>(start + 5)?.into());
        fields.insert(
            "exit_code".into(),
            serde_json::to_value(row.get::<_, Option<i64>>(start + 6)?)?,
        );
        fields.insert(
            "signal".into(),
            serde_json::to_value(row.get::<_, Option<i32>>(start + 7)?)?,
        );
        Ok(serde_json::from_value(value)?)
    };
    decode().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(start, rusqlite::types::Type::Text, error.into())
    })
}
fn payload(job: &Job) -> Result<String> {
    let mut value = serde_json::to_value(job)?;
    let fields = value
        .as_object_mut()
        .context("task payload must be an object")?;
    for name in JOB_COLUMNS.split(',').skip(1) {
        fields.remove(name);
    }
    Ok(serde_json::to_string(&value)?)
}
fn state_name(state: &JobState) -> Result<String> {
    Ok(serde_json::from_value(serde_json::to_value(state)?)?)
}
pub(super) fn job_at(db: &Connection, id: &str) -> Result<Option<Job>> {
    Ok(db
        .query_row(
            &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE id=?1"),
            [id],
            |row| read_job(row, 0),
        )
        .optional()?)
}
pub(super) fn write_job(db: &Connection, job: &Job) -> Result<()> {
    db.execute(
        "UPDATE jobs SET data=?2,state=?3,last_seq=?4,output_complete=?5,incomplete_reason=?6,
        updated_at_ms=?7,exit_code=?8,signal=?9 WHERE id=?1",
        params![
            job.job_id,
            payload(job)?,
            state_name(&job.state)?,
            i64::try_from(job.last_seq)?,
            job.output_complete,
            job.incomplete_reason,
            job.updated_at_ms,
            job.exit_code,
            job.signal
        ],
    )?;
    Ok(())
}

/// Known loss is cumulative: expiry outranks truncation, then capture/pipe loss.
pub(crate) fn merge_incomplete(previous: &mut Option<String>, next: Option<String>) {
    fn priority(reason: &str) -> u8 {
        if reason == "LOG_EXPIRED" {
            4
        } else if reason == "TRUNCATED" {
            3
        } else if reason.starts_with("CAPTURE_ERROR") {
            2
        } else {
            1
        }
    }
    if let Some(next) = next
        && previous
            .as_ref()
            .is_none_or(|old| priority(&next) > priority(old))
    {
        *previous = Some(next);
    }
}
#[derive(Clone)]
pub struct JobOutcome {
    pub state: JobState,
    pub exit_code: Option<i64>,
    pub signal: Option<i32>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
    pub incomplete_reason: Option<String>,
    pub leftover_possible: bool,
}
impl Database {
    pub(super) fn open(path: &Path, create: bool) -> Result<Self> {
        let mut db = super::open(path, create)?;
        crate::database::initialize(&mut db, "tasks", TASK_SCHEMA_VERSION, TASK_SCHEMA)?;
        let db_id = db
            .query_row("SELECT value FROM meta WHERE key='db_id'", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        let db_id = match db_id {
            Some(id) => id,
            None if create => {
                let id = format!("db_{}", uuid::Uuid::new_v4().simple());
                db.execute("INSERT INTO meta VALUES('db_id',?1)", [&id])?;
                id
            }
            None => bail!(ErrorCode::DbCorrupt.error("missing db_id")),
        };
        Ok(Self {
            db,
            db_id,
            changes: tokio::sync::watch::channel(()).0,
        })
    }
    pub(super) fn get(&self, id: &str) -> Result<Option<Job>> {
        job_at(&self.db, id)
    }
    pub(super) fn by_request(&self, source: &str, id: &str) -> Result<Option<Job>> {
        Ok(self
            .db
            .query_row(
                &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE source=?1 AND request_id=?2"),
                params![source, id],
                |row| read_job(row, 0),
            )
            .optional()?)
    }
    pub(super) fn all(&self) -> Result<Vec<Job>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs ORDER BY rowid DESC"
        ))?;
        Ok(statement
            .query_map([], |row| read_job(row, 0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub(super) fn active_count(&self) -> Result<usize> {
        let count: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM jobs WHERE state IN ('starting','running')",
            [],
            |row| row.get(0),
        )?;
        Ok(count.try_into()?)
    }
    pub(super) fn page(
        &self,
        source: &str,
        running: bool,
        request: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Job>> {
        if let Some(request) = request {
            return Ok(self
                .by_request(source, request)?
                .into_iter()
                .filter(|job| !running || !job.state.terminal())
                .skip(offset)
                .take(limit.min(1000))
                .collect());
        }
        let sql = format!("SELECT {JOB_COLUMNS} FROM jobs WHERE source=?1 AND (?2=0 OR state IN ('starting','running'))
            ORDER BY rowid DESC LIMIT ?3 OFFSET ?4");
        let mut statement = self.db.prepare(&sql)?;
        Ok(statement
            .query_map(
                params![
                    source,
                    running,
                    limit.min(1000) as i64,
                    i64::try_from(offset).unwrap_or(i64::MAX)
                ],
                |row| read_job(row, 0),
            )?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub(super) fn insert(&self, job: &Job) -> Result<()> {
        self.db.execute(
            "INSERT INTO jobs VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                job.job_id,
                job.source_device_id,
                job.request_id,
                payload(job)?,
                state_name(&job.state)?,
                i64::try_from(job.last_seq)?,
                job.output_complete,
                job.incomplete_reason,
                job.updated_at_ms,
                job.exit_code,
                job.signal
            ],
        )?;
        self.changes.send_replace(());
        Ok(())
    }
    pub(super) fn update(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut Job) -> Result<()>,
    ) -> Result<Job> {
        let transaction = self.db.transaction()?;
        let mut job = job_at(&transaction, id)?
            .context(ErrorCode::JobNotFound.error("unknown accepted task"))?;
        if job.state.terminal() {
            return Ok(job);
        }
        change(&mut job)?;
        job.updated_at_ms = now_ms();
        write_job(&transaction, &job)?;
        transaction.commit()?;
        self.changes.send_replace(());
        Ok(job)
    }
    pub(super) fn record_process(&mut self, id: &str, process: ProcessIdentity) -> Result<Job> {
        self.update(id, |job| {
            if job.state != JobState::Starting || job.process.is_some() {
                bail!(ErrorCode::InvalidRequest.error("process already recorded"));
            }
            job.process = Some(process);
            Ok(())
        })
    }
    pub(super) fn mark_running(&mut self, id: &str) -> Result<Job> {
        self.update(id, |job| {
            if job.state != JobState::Starting || job.process.is_none() {
                bail!(ErrorCode::InvalidRequest.error("running requires a recorded process"));
            }
            job.state = JobState::Running;
            Ok(())
        })
    }
    pub(super) fn finish(&mut self, id: &str, outcome: JobOutcome) -> Result<Job> {
        if !outcome.state.terminal() {
            bail!(ErrorCode::InvalidRequest.error("finish requires a terminal state"));
        }
        self.update(id, |job| {
            job.state = outcome.state;
            job.exit_code = outcome.exit_code;
            job.signal = outcome.signal;
            job.duration_ms = outcome.duration_ms;
            job.error = outcome.error;
            job.leftover_possible = outcome.leftover_possible;
            merge_incomplete(&mut job.incomplete_reason, outcome.incomplete_reason);
            job.output_complete &= job.incomplete_reason.is_none();
            Ok(())
        })
    }
    pub(super) fn replace_fixture(&self, job: &Job) -> Result<()> {
        write_job(&self.db, job)?;
        self.changes.send_replace(());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn job(store: &TaskStore) -> Job {
        Job {
            job_id: "TEST01".into(),
            request_id: "request".into(),
            request_hash: "hash".into(),
            source_device_id: "source".into(),
            target_device_id: "target".into(),
            db_id: store.db_id.clone(),
            program: "test".into(),
            args: vec![],
            cwd: "/".into(),
            state: JobState::Starting,
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
    fn outcome(state: JobState, reason: Option<&str>) -> JobOutcome {
        JobOutcome {
            state,
            exit_code: Some(0),
            signal: None,
            duration_ms: Some(1),
            error: None,
            incomplete_reason: reason.map(str::to_owned),
            leftover_possible: false,
        }
    }
    #[test]
    fn lifecycle_preserves_sequences_loss_and_terminal_states() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = TaskStore::open(&dir.path().join("tasks.db"), true)?;
        store.insert(&job(&store))?;
        store.append("TEST01", "stdout", b"first")?;
        store.worker.call(|db| {
            db.db
                .execute("UPDATE log_sizes SET bytes=?1", [MAX_JOB_LOG_BYTES])?;
            Ok(())
        })?;
        assert_eq!(store.append("TEST01", "stderr", b"truncated")?, None);
        let finished =
            store.finish_sync("TEST01", outcome(JobState::Exited, Some("DETACHED_OUTPUT")))?;
        assert_eq!(finished.last_seq, 1);
        assert_eq!(finished.incomplete_reason.as_deref(), Some("TRUNCATED"));
        assert!(!finished.output_complete);
        let changes = store.subscribe();
        store.finish_sync("TEST01", outcome(JobState::Failed, None))?;
        assert!(!changes.has_changed()?);
        assert_eq!(store.append("TEST01", "stdout", b"late")?, None);
        assert_eq!(store.get("TEST01")?.unwrap().state, JobState::Exited);
        assert_eq!(store.get("TEST01")?.unwrap().last_seq, 1);
        store.worker.call(|database| {
            database.db.execute(
                "UPDATE jobs SET updated_at_ms=?1 WHERE id='TEST01'",
                [now_ms() - FINISHED_LOG_RETENTION_MS - 1],
            )?;
            Ok(())
        })?;
        store.prune()?;
        let late = store.finish_sync("TEST01", outcome(JobState::Failed, None))?;
        assert_eq!(late.incomplete_reason.as_deref(), Some("LOG_EXPIRED"));
        assert!(!late.output_complete);
        assert_eq!(late.last_seq, 1);
        assert!(store.logs("TEST01", 0)?.is_empty());
        Ok(())
    }
    #[test]
    fn failed_append_rolls_back_counters_and_does_not_notify() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = TaskStore::open(&dir.path().join("tasks.db"), true)?;
        store.insert(&job(&store))?;
        let changes = store.subscribe();
        store.worker.call(|db| {
            db.db.execute_batch("CREATE TRIGGER reject_log BEFORE INSERT ON logs BEGIN SELECT RAISE(FAIL,'injected disk failure'); END;")?;
            Ok(())
        })?;
        assert!(store.append("TEST01", "stdout", b"never visible").is_err());
        assert!(!changes.has_changed()?);
        assert_eq!(store.get("TEST01")?.unwrap().last_seq, 0);
        assert!(store.logs("TEST01", 0)?.is_empty());
        assert_eq!(
            store.worker.call(|db| Ok(db.db.query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key='log_bytes'",
                [],
                |row| row.get::<_, i64>(0)
            )?))?,
            0
        );
        Ok(())
    }
    #[tokio::test]
    async fn canceled_reader_writes_are_flushed_before_finish() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = std::sync::Arc::new(TaskStore::open(&dir.path().join("tasks.db"), true)?);
        store.insert(&job(&store))?;
        let (started, running) = tokio::sync::oneshot::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        let worker = store.clone();
        let blocker = tokio::spawn(async move {
            worker
                .worker
                .query(move |_| {
                    let _ = started.send(());
                    paused.recv()?;
                    Ok(())
                })
                .await
        });
        running.await?;
        {
            let write = store.append_async("TEST01", "stdout", b"queued".to_vec());
            tokio::pin!(write);
            assert!(matches!(
                futures_util::poll!(&mut write),
                std::task::Poll::Pending
            ));
            // Drop the waiting reader after its write has entered the queue.
        }
        resume.send(())?;
        store.flush().await?;
        blocker.await??;
        let finished = store
            .finish("TEST01", outcome(JobState::Canceled, None))
            .await?;
        assert_eq!(finished.last_seq, 1);
        assert_eq!(store.logs("TEST01", 0)?.len(), 1);
        Ok(())
    }
}
