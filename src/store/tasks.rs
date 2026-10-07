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

const MAX_DIAGNOSTIC_BYTES: usize = 1024;

pub(super) fn bound_diagnostic(text: &mut String) {
    if text.len() <= MAX_DIAGNOSTIC_BYTES {
        return;
    }
    const SUFFIX: &str = " [diagnostic truncated]";
    let mut end = MAX_DIAGNOSTIC_BYTES - SUFFIX.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(SUFFIX);
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
    if let Some(previous) = previous.as_mut() {
        bound_diagnostic(previous);
    }
    if let Some(mut next) = next
        && previous
            .as_ref()
            .is_none_or(|old| priority(&next) > priority(old))
    {
        bound_diagnostic(&mut next);
        *previous = Some(next);
    }
}
#[derive(Clone)]
pub(crate) struct JobOutcome {
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
    pub(super) fn unfinished(&self) -> Result<Vec<Job>> {
        let mut statement = self.db.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE state IN ('starting','running')"
        ))?;
        Ok(statement
            .query_map([], |row| read_job(row, 0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub(super) fn unfinished_ids(&self) -> Result<Vec<String>> {
        let mut statement = self
            .db
            .prepare("SELECT id FROM jobs WHERE state IN ('starting','running')")?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
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
        let active = if running {
            "AND state IN ('starting','running')"
        } else {
            ""
        };
        let sql = format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE source=?1 {active}
            ORDER BY rowid DESC LIMIT ?2 OFFSET ?3"
        );
        let mut statement = self.db.prepare(&sql)?;
        Ok(statement
            .query_map(
                params![
                    source,
                    limit.min(1000) as i64,
                    i64::try_from(offset).unwrap_or(i64::MAX)
                ],
                |row| read_job(row, 0),
            )?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub(super) fn page_bounded(
        &self,
        source: &str,
        running: bool,
        request: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<Job>, Option<usize>)> {
        let active = if running {
            "AND state IN ('starting','running')"
        } else {
            ""
        };
        let mut statement = self.db.prepare(&format!(
            "SELECT {JOB_COLUMNS} FROM jobs WHERE source=?1 {active}
             AND (?2 IS NULL OR request_id=?2) ORDER BY rowid DESC LIMIT ?3 OFFSET ?4"
        ))?;
        let rows = statement.query_map(
            params![
                source,
                request,
                limit.min(1000) as i64,
                i64::try_from(offset).unwrap_or(i64::MAX)
            ],
            |row| read_job(row, 0),
        )?;
        // Account for the JSON envelope, commas and the continuation offset.
        let mut bytes = 128;
        let mut jobs = vec![];
        for row in rows {
            let job = row?;
            let size = serde_json::to_vec(&job)?.len() + 1;
            if bytes + size > MAX_MESSAGE {
                if jobs.is_empty() {
                    bail!(ErrorCode::MessageTooLarge.error("single task exceeds response budget"));
                }
                let next = offset
                    .checked_add(jobs.len())
                    .context("task offset overflow")?;
                return Ok((jobs, Some(next)));
            }
            bytes += size;
            jobs.push(job);
        }
        Ok((jobs, None))
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
        // Reserve the writer before reading; another connection cannot invalidate
        // this snapshot while a lifecycle update is being prepared.
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
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
            if let Some(error) = job.error.as_mut() {
                bound_diagnostic(error);
            }
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

    #[tokio::test]
    async fn diagnostic_limits_preserve_utf8_and_loss_precedence() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = TaskStore::open(&dir.path().join("tasks.db"), true)?;
        let mut initial = job(&store);
        initial.incomplete_reason = Some(format!("CAPTURE_ERROR: {}", "\u{1}".repeat(4096)));
        initial.output_complete = false;
        store.insert(&initial)?;
        store.append("TEST01", "stdout", b"retained output")?;
        assert!(
            store
                .get("TEST01")?
                .unwrap()
                .incomplete_reason
                .unwrap()
                .len()
                <= MAX_DIAGNOSTIC_BYTES
        );
        let mut finished = outcome(JobState::Failed, None);
        finished.error = Some(format!("EXECUTION_ERROR: {}", "界".repeat(2048)));
        let job = store.finish("TEST01", finished).await?;
        for text in [job.error.as_ref(), job.incomplete_reason.as_ref()] {
            let text = text.unwrap();
            assert!(text.len() <= MAX_DIAGNOSTIC_BYTES);
            assert!(text.ends_with(" [diagnostic truncated]"));
        }
        assert!(job.error.unwrap().starts_with("EXECUTION_ERROR:"));
        let mut reason = job.incomplete_reason;
        assert!(reason.as_ref().unwrap().starts_with("CAPTURE_ERROR:"));
        merge_incomplete(&mut reason, Some("DETACHED_OUTPUT".into()));
        assert!(reason.as_ref().unwrap().starts_with("CAPTURE_ERROR:"));
        merge_incomplete(&mut reason, Some("TRUNCATED".into()));
        assert_eq!(reason.as_deref(), Some("TRUNCATED"));
        merge_incomplete(&mut reason, Some("LOG_EXPIRED".into()));
        assert_eq!(reason.as_deref(), Some("LOG_EXPIRED"));
        Ok(())
    }

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
    #[tokio::test]
    async fn recovery_and_shutdown_skip_terminal_history_and_keep_old_unfinished_jobs() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let store = TaskStore::open(&directory.path().join("tasks.db"), true)?;
        let mut pending = job(&store);
        pending.state = JobState::Starting;
        pending.created_at_ms = 1;
        pending.updated_at_ms = 1;
        store.insert(&pending)?;
        let mut running = pending.clone();
        running.job_id = "TEST02".into();
        running.request_id = "running".into();
        running.state = JobState::Running;
        store.insert(&running)?;
        store.worker.call(|database| {
            // Invalid historical payloads prove recovery never decodes terminal rows.
            database.db.execute_batch(
                "WITH RECURSIVE history(n) AS (
                    SELECT 1 UNION ALL SELECT n+1 FROM history WHERE n<100000
                ) INSERT INTO jobs
                SELECT printf('old-%d',n),'source',printf('old-%d',n),'not JSON',
                    'exited',0,1,NULL,1,0,NULL FROM history;",
            )?;
            for sql in [
                "SELECT id FROM jobs WHERE state IN ('starting','running')",
                "SELECT COUNT(*) FROM jobs WHERE state IN ('starting','running')",
            ] {
                let mut statement = database.db.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
                let plan = statement
                    .query_map([], |row| row.get::<_, String>(3))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                assert!(
                    plan.iter().any(|detail| detail.contains("jobs_active")),
                    "{plan:?}"
                );
            }
            Ok(())
        })?;
        assert_eq!(store.active_count()?, 2);
        let mut recovered = store.unfinished()?;
        recovered.sort_by(|left, right| left.job_id.cmp(&right.job_id));
        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered[0].state, JobState::Starting);
        assert_eq!(recovered[1].state, JobState::Running);
        assert!(recovered.iter().all(|job| job.created_at_ms == 1));
        let mut ids = store.unfinished_ids().await?;
        ids.sort();
        assert_eq!(ids, ["TEST01", "TEST02"]);
        store.finish_sync("TEST01", outcome(JobState::Lost, None))?;
        store.prune()?;
        assert_eq!(
            store
                .by_request(&pending.source_device_id, &pending.request_id)?
                .unwrap()
                .job_id,
            "TEST01"
        );
        assert_eq!(store.unfinished_ids().await?, ["TEST02"]);
        assert_eq!(
            store.worker.call(|database| Ok(database.db.query_row(
                "SELECT COUNT(*) FROM jobs",
                [],
                |row| row.get::<_, i64>(0)
            )?))?,
            100002
        );
        Ok(())
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
    #[test]
    fn lifecycle_update_reserves_the_writer_before_reading() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("tasks.db");
        let store = TaskStore::open(&path, true)?;
        store.insert(&job(&store))?;
        let competing_writer = Connection::open(&path)?;
        competing_writer.busy_timeout(std::time::Duration::ZERO)?;
        let updated = store.worker.call(move |database| {
            database.update("TEST01", |job| {
                // This step runs after the task snapshot was read. An external
                // writer must wait rather than making that snapshot stale.
                let write =
                    competing_writer.execute("UPDATE jobs SET last_seq=999 WHERE id='TEST01'", []);
                anyhow::ensure!(
                    matches!(write, Err(rusqlite::Error::SqliteFailure(error, _))
                        if error.code == rusqlite::ErrorCode::DatabaseBusy),
                    "another writer changed the task during its lifecycle update"
                );
                job.state = JobState::Canceled;
                Ok(())
            })
        })?;
        assert_eq!(updated.state, JobState::Canceled);
        assert_eq!(updated.last_seq, 0);
        assert_eq!(store.get("TEST01")?.unwrap().last_seq, 0);
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
