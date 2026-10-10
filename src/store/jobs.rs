//! One durable row per business operation; lifecycle and output have one owner.
use super::*;

pub(crate) const JOB_COLUMNS: &str = concat!(
    "job_id,source_device_id,target_device_id,request_id,request_hash,kind,params_json,result_json,state,",
    "error_code,error_message,created_at_ms,started_at_ms,finished_at_ms,updated_at_ms,process_json,",
    "leftover_possible,last_log_seq,log_bytes,output_complete,output_loss_reason,",
    "(SELECT value FROM meta WHERE key='db_id')"
);
pub(crate) const JOB_SCHEMA_VERSION: i64 = 2;
const JOB_SCHEMA: &str = include_str!("schema.sql");

pub(crate) fn prepare_upgrade(path: &Path) -> Result<()> {
    if crate::database::older_schema(path, JOB_SCHEMA_VERSION)? {
        let mut db = super::open(path, false)?;
        crate::database::initialize_with_backup(
            &mut db,
            path,
            "jobs",
            JOB_SCHEMA_VERSION,
            JOB_SCHEMA,
        )?;
    }
    Ok(())
}

pub(super) struct Database {
    pub(super) db: Connection,
    pub(super) db_id: String,
    pub(super) changes: tokio::sync::watch::Sender<()>,
    cache: crate::attachments::Cache,
}
pub(crate) fn read_job(row: &rusqlite::Row<'_>, start: usize) -> rusqlite::Result<Job> {
    let decode = || -> Result<Job> {
        let details: JobDetails = serde_json::from_value(serde_json::json!({
            "kind":row.get::<_,String>(start+5)?,
            "params":serde_json::from_str::<serde_json::Value>(&row.get::<_,String>(start+6)?)?
        }))?;
        let result = row
            .get::<_, Option<String>>(start + 7)?
            .map(|value| serde_json::from_str::<JobResult>(&value))
            .transpose()?;
        if result
            .as_ref()
            .is_some_and(|value| !value.matches(details.kind()))
        {
            bail!("result does not match job kind");
        }
        Ok(Job {
            job_id: row.get(start)?,
            source_device_id: row.get(start + 1)?,
            target_device_id: row.get(start + 2)?,
            request_id: row.get(start + 3)?,
            request_hash: row.get(start + 4)?,
            details,
            result,
            state: serde_json::from_value(row.get::<_, String>(start + 8)?.into())?,
            error_code: row.get(start + 9)?,
            error_message: row.get(start + 10)?,
            created_at_ms: row.get(start + 11)?,
            started_at_ms: row.get(start + 12)?,
            finished_at_ms: row.get(start + 13)?,
            updated_at_ms: row.get(start + 14)?,
            process: row
                .get::<_, Option<String>>(start + 15)?
                .map(|value| serde_json::from_str(&value))
                .transpose()?,
            leftover_possible: row.get(start + 16)?,
            last_log_seq: u64::try_from(row.get::<_, i64>(start + 17)?)?,
            log_bytes: u64::try_from(row.get::<_, i64>(start + 18)?)?,
            output_complete: row.get(start + 19)?,
            output_loss_reason: row.get(start + 20)?,
            db_id: row.get(start + 21)?,
            attachments: vec![],
        })
    };
    decode().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(start, rusqlite::types::Type::Text, error.into())
    })
}
fn saved_params(details: &JobDetails) -> Result<String> {
    Ok(serde_json::to_string(
        &serde_json::to_value(details)?["params"],
    )?)
}
fn state_name(state: &JobState) -> Result<String> {
    Ok(serde_json::from_value(serde_json::to_value(state)?)?)
}
pub(crate) fn job_at(db: &Connection, id: &str) -> Result<Option<Job>> {
    Ok(db
        .query_row(
            &format!("SELECT {JOB_COLUMNS} FROM jobs WHERE job_id=?1"),
            [id],
            |row| read_job(row, 0),
        )
        .optional()?)
}
pub(super) fn write_job(db: &Connection, job: &Job) -> Result<()> {
    db.execute(
        "UPDATE jobs SET state=?2,result_json=?3,error_code=?4,error_message=?5,
        started_at_ms=?6,finished_at_ms=?7,
        updated_at_ms=?8,process_json=?9,leftover_possible=?10,last_log_seq=?11,log_bytes=?12,
        output_complete=?13,output_loss_reason=?14 WHERE job_id=?1",
        params![
            job.job_id,
            state_name(&job.state)?,
            job.result.as_ref().map(serde_json::to_string).transpose()?,
            job.error_code,
            job.error_message,
            job.started_at_ms,
            job.finished_at_ms,
            job.updated_at_ms,
            job.process
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            job.leftover_possible,
            i64::try_from(job.last_log_seq)?,
            i64::try_from(job.log_bytes)?,
            job.output_complete,
            job.output_loss_reason
        ],
    )?;
    Ok(())
}
const MAX_DIAGNOSTIC_BYTES: usize = 1024;
pub(crate) fn bound_diagnostic(text: &mut String) {
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
/// Known output loss is cumulative, including after terminal state.
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
    pub result: Option<JobResult>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub output_loss_reason: Option<String>,
    pub leftover_possible: bool,
}
impl JobOutcome {
    pub(crate) fn new(state: JobState) -> Self {
        Self {
            state,
            result: None,
            error_code: None,
            error_message: None,
            output_loss_reason: None,
            leftover_possible: false,
        }
    }
    pub(crate) fn failed(error: &anyhow::Error) -> Self {
        let (code, message) = crate::error::wire(error);
        Self {
            error_code: Some(code),
            error_message: Some(message),
            ..Self::new(JobState::Failed)
        }
    }
}
impl Database {
    pub(super) fn open(path: &Path, create: bool) -> Result<Self> {
        let mut db = super::open(path, create)?;
        crate::database::initialize(&mut db, "jobs", JOB_SCHEMA_VERSION, JOB_SCHEMA)?;
        let generation = db
            .query_row("SELECT value FROM meta WHERE key='db_id'", [], |row| {
                row.get::<_, String>(0)
            })
            .optional()?;
        let db_id = match generation {
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
            cache: crate::attachments::Cache::for_database(path),
        })
    }
    fn hydrate(&self, mut job: Job) -> Result<Job> {
        job.attachments = crate::attachments::for_job(&self.db, &self.cache, &job.job_id)?;
        Ok(job)
    }
    pub(super) fn get(&self, id: &str) -> Result<Option<Job>> {
        job_at(&self.db, id)?
            .map(|job| self.hydrate(job))
            .transpose()
    }
    pub(super) fn by_request(&self, source: &str, request: &str) -> Result<Option<Job>> {
        self.db
            .query_row(
                &format!(
                    "SELECT {JOB_COLUMNS} FROM jobs WHERE source_device_id=?1 AND request_id=?2"
                ),
                params![source, request],
                |row| read_job(row, 0),
            )
            .optional()?
            .map(|job| self.hydrate(job))
            .transpose()
    }
    pub(super) fn all(&self) -> Result<Vec<Job>> {
        self.db
            .prepare(&format!(
                "SELECT {JOB_COLUMNS} FROM jobs ORDER BY created_at_ms DESC,job_id DESC"
            ))?
            .query_map([], |row| read_job(row, 0))?
            .map(|job| self.hydrate(job?))
            .collect()
    }
    pub(super) fn unfinished(&self) -> Result<Vec<Job>> {
        Ok(self
            .db
            .prepare(&format!(
                "SELECT {JOB_COLUMNS} FROM jobs WHERE state IN ('accepted','running')"
            ))?
            .query_map([], |row| read_job(row, 0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub(super) fn unfinished_ids(&self) -> Result<Vec<String>> {
        Ok(self
            .db
            .prepare("SELECT job_id FROM jobs WHERE state IN ('accepted','running')")?
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub(super) fn active_count(&self) -> Result<usize> {
        Ok(self
            .db
            .query_row(
                "SELECT COUNT(*) FROM jobs WHERE kind IN ('exec','stream_exec')
            AND state IN ('accepted','running')",
                [],
                |row| row.get::<_, i64>(0),
            )?
            .try_into()?)
    }
    pub(super) fn page(
        &self,
        source: &str,
        running: bool,
        request: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Job>> {
        let active = if running {
            "AND state IN ('accepted','running')"
        } else {
            ""
        };
        self.db.prepare(&format!("SELECT {JOB_COLUMNS} FROM jobs WHERE source_device_id=?1 {active}
            AND (?2 IS NULL OR request_id=?2) ORDER BY created_at_ms DESC,job_id DESC LIMIT ?3 OFFSET ?4"))?
            .query_map(params![source,request,limit.min(1000) as i64,i64::try_from(offset)?],|row|read_job(row,0))?
            .map(|job|self.hydrate(job?)).collect()
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
            "AND state IN ('accepted','running')"
        } else {
            ""
        };
        let mut statement=self.db.prepare(&format!("SELECT {JOB_COLUMNS} FROM jobs WHERE source_device_id=?1 {active}
            AND (?2 IS NULL OR request_id=?2) ORDER BY created_at_ms DESC,job_id DESC LIMIT ?3 OFFSET ?4"))?;
        let rows = statement.query_map(
            params![
                source,
                request,
                limit.min(1000) as i64,
                i64::try_from(offset)?
            ],
            |row| read_job(row, 0),
        )?;
        let mut bytes = 128;
        let mut jobs = vec![];
        for job in rows {
            let job = self.hydrate(job?)?;
            let size = serde_json::to_vec(&job)?.len() + 1;
            if bytes + size > MAX_MESSAGE {
                if jobs.is_empty() {
                    bail!(ErrorCode::MessageTooLarge.error("single job exceeds response budget"));
                }
                let next = offset
                    .checked_add(jobs.len())
                    .context("job offset overflow")?;
                return Ok((jobs, Some(next)));
            }
            bytes += size;
            jobs.push(job);
        }
        Ok((jobs, None))
    }
    pub(super) fn insert(&self, job: &Job) -> Result<()> {
        validate_job_size(job)?;
        self.db.execute("INSERT INTO jobs(job_id,source_device_id,target_device_id,request_id,request_hash,kind,params_json,result_json,state,
            error_code,error_message,created_at_ms,started_at_ms,finished_at_ms,updated_at_ms,process_json,
            leftover_possible,last_log_seq,log_bytes,output_complete,output_loss_reason)
            VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
            params![
                job.job_id,job.source_device_id,job.target_device_id,job.request_id,job.request_hash,
                job.kind().as_str(),saved_params(&job.details)?,
                job.result.as_ref().map(serde_json::to_string).transpose()?,state_name(&job.state)?,job.error_code,job.error_message,
                job.created_at_ms,job.started_at_ms,job.finished_at_ms,job.updated_at_ms,job.process.as_ref().map(serde_json::to_string).transpose()?,
                job.leftover_possible,i64::try_from(job.last_log_seq)?,i64::try_from(job.log_bytes)?,job.output_complete,job.output_loss_reason])?;
        self.changes.send_replace(());
        Ok(())
    }
    pub(super) fn update(
        &mut self,
        id: &str,
        change: impl FnOnce(&mut Job) -> Result<()>,
    ) -> Result<Job> {
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut job = job_at(&transaction, id)?
            .context(ErrorCode::JobNotFound.error("unknown accepted job"))?;
        if job.state.terminal() {
            return Ok(job);
        }
        change(&mut job)?;
        job.updated_at_ms = now_ms();
        write_job(&transaction, &job)?;
        transaction.commit()?;
        self.changes.send_replace(());
        self.hydrate(job)
    }
    pub(super) fn record_process(&mut self, id: &str, process: ProcessIdentity) -> Result<Job> {
        self.update(id, |job| {
            if job.state != JobState::Accepted
                || job.process.is_some()
                || job.details.command().is_none()
            {
                bail!(
                    ErrorCode::InvalidRequest
                        .error("process identity cannot be recorded for this job")
                );
            }
            job.process = Some(process);
            Ok(())
        })
    }
    pub(super) fn mark_running(&mut self, id: &str) -> Result<Job> {
        self.update(id, |job| {
            if job.state != JobState::Accepted
                || (job.details.command().is_some() && job.process.is_none())
            {
                bail!(ErrorCode::InvalidRequest.error("job is not ready to start"));
            }
            job.state = JobState::Running;
            job.started_at_ms = Some(now_ms());
            Ok(())
        })
    }
    pub(super) fn finish(&mut self, id: &str, outcome: JobOutcome) -> Result<Job> {
        if !outcome.state.terminal() {
            bail!(ErrorCode::InvalidRequest.error("finish requires a terminal state"));
        }
        self.update(id, |job| {
            if outcome.state == JobState::Succeeded && (outcome.result.is_none() ||
                matches!(&outcome.result, Some(JobResult::Command(result)) if result.exit_code != Some(0) || result.signal.is_some())) {
                bail!(ErrorCode::InvalidRequest.error("successful jobs require a confirmed successful result"));
            }
            if outcome
                .result
                .as_ref()
                .is_some_and(|result| !result.matches(job.kind()))
            {
                bail!(ErrorCode::InvalidRequest.error("result does not match job kind"));
            }
            job.state = outcome.state;
            job.result = outcome.result;
            job.error_code = outcome.error_code;
            job.error_message = outcome.error_message;
            if let Some(error) = job.error_message.as_mut() {
                bound_diagnostic(error);
            }
            job.finished_at_ms = Some(now_ms());
            job.leftover_possible = outcome.leftover_possible;
            merge_incomplete(&mut job.output_loss_reason, outcome.output_loss_reason);
            if let Some(complete) = job.output_complete.as_mut() {
                *complete &= job.output_loss_reason.is_none();
            }
            Ok(())
        })
    }
    pub(super) fn attach(
        &mut self,
        id: &str,
        metadata: crate::attachments::AttachmentMetadata,
    ) -> Result<()> {
        let transaction = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let kind: String =
            transaction.query_row("SELECT kind FROM jobs WHERE job_id=?1", [id], |row| {
                row.get(0)
            })?;
        if !["push", "pull", "screenshot"].contains(&kind.as_str()) {
            bail!(ErrorCode::InvalidRequest.error("this job does not produce retained files"));
        }
        transaction.execute(
            "INSERT INTO job_attachments VALUES(?1,?2,?3,?4,?5,?6,'available',NULL)",
            params![
                metadata.id,
                id,
                metadata.name,
                i64::try_from(metadata.size)?,
                metadata.sha256,
                metadata.created_at_ms
            ],
        )?;
        transaction.execute(
            "UPDATE jobs SET updated_at_ms=?2 WHERE job_id=?1",
            params![id, now_ms()],
        )?;
        transaction.commit()?;
        self.changes.send_replace(());
        Ok(())
    }
    pub(super) fn replace_fixture(&self, job: &Job) -> Result<()> {
        self.db.execute(
            "UPDATE jobs SET params_json=?2 WHERE job_id=?1",
            params![job.job_id, saved_params(&job.details)?],
        )?;
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
        let store = JobStore::open(&dir.path().join("tasks.db"), true)?;
        let mut initial = job(&store);
        initial.output_loss_reason = Some(format!("CAPTURE_ERROR: {}", "\u{1}".repeat(4096)));
        initial.output_complete = Some(false);
        store.insert(&initial)?;
        store.append("TEST01", "stdout", b"retained output")?;
        assert!(
            store
                .get("TEST01")?
                .unwrap()
                .output_loss_reason
                .unwrap()
                .len()
                <= MAX_DIAGNOSTIC_BYTES
        );
        let mut finished = outcome(JobState::Failed, None);
        finished.error_message = Some(format!("EXECUTION_ERROR: {}", "界".repeat(2048)));
        let job = store.finish("TEST01", finished).await?;
        for text in [job.error_message.as_ref(), job.output_loss_reason.as_ref()] {
            let text = text.unwrap();
            assert!(text.len() <= MAX_DIAGNOSTIC_BYTES);
            assert!(text.ends_with(" [diagnostic truncated]"));
        }
        assert!(job.error_message.unwrap().starts_with("EXECUTION_ERROR:"));
        let mut reason = job.output_loss_reason;
        assert!(reason.as_ref().unwrap().starts_with("CAPTURE_ERROR:"));
        merge_incomplete(&mut reason, Some("DETACHED_OUTPUT".into()));
        assert!(reason.as_ref().unwrap().starts_with("CAPTURE_ERROR:"));
        merge_incomplete(&mut reason, Some("TRUNCATED".into()));
        assert_eq!(reason.as_deref(), Some("TRUNCATED"));
        merge_incomplete(&mut reason, Some("LOG_EXPIRED".into()));
        assert_eq!(reason.as_deref(), Some("LOG_EXPIRED"));
        Ok(())
    }

    fn job(store: &JobStore) -> Job {
        Job {
            job_id: "TEST01".into(),
            request_id: "request".into(),
            request_hash: "hash".into(),
            source_device_id: "source".into(),
            target_device_id: "target".into(),
            db_id: store.db_id.clone(),
            state: JobState::Accepted,
            last_log_seq: 0,
            output_loss_reason: None,
            created_at_ms: now_ms(),
            updated_at_ms: now_ms(),
            leftover_possible: false,
            process: None,
            details: JobDetails::Exec(CommandParams {
                program: "test".into(),
                args: vec![],
                cwd: "/".into(),
                timeout: 0,
                shell: None,
                input_size: None,
                input_sha256: None,
            }),
            result: None,
            output_complete: Some(true),
            error_code: None,
            error_message: None,
            log_bytes: 0,
            attachments: vec![],
            started_at_ms: None,
            finished_at_ms: if (JobState::Accepted).terminal() {
                Some(now_ms())
            } else {
                None
            },
        }
    }
    fn outcome(state: JobState, reason: Option<&str>) -> JobOutcome {
        JobOutcome {
            state,
            output_loss_reason: reason.map(str::to_owned),
            leftover_possible: false,
            result: Some(JobResult::Command(CommandResult {
                exit_code: Some(0),
                signal: None,
                duration_ms: 1,
                ..Default::default()
            })),
            error_code: None,
            error_message: None,
        }
    }
    #[tokio::test]
    async fn recovery_and_shutdown_skip_terminal_history_and_keep_old_unfinished_jobs() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let store = JobStore::open(&directory.path().join("tasks.db"), true)?;
        let mut pending = job(&store);
        pending.state = JobState::Accepted;
        pending.created_at_ms = 1;
        pending.updated_at_ms = 1;
        store.insert(&pending)?;
        let mut running = pending.clone();
        running.job_id = "TEST02".into();
        running.request_id = "running".into();
        running.state = JobState::Running;
        store.insert(&running)?;
        store.worker.call(|database| {
            // Undecodable terminal parameters prove recovery reads only unfinished rows.
            database.db.execute_batch(
                "WITH RECURSIVE history(n) AS (
                    SELECT 1 UNION ALL SELECT n+1 FROM history WHERE n<100000
                ) INSERT INTO jobs
                (job_id,source_device_id,target_device_id,request_id,request_hash,kind,params_json,state,created_at_ms,updated_at_ms)
                SELECT printf('old-%d',n),'source','target',printf('old-%d',n),'hash',
                    'exec','{}','succeeded',1,1 FROM history;",
            )?;
            for sql in [
                "SELECT job_id FROM jobs WHERE state IN ('accepted','running')",
                "SELECT COUNT(*) FROM jobs WHERE state IN ('accepted','running')",
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
        assert_eq!(recovered[0].state, JobState::Accepted);
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
        let store = JobStore::open(&dir.path().join("tasks.db"), true)?;
        store.insert(&job(&store))?;
        store.append("TEST01", "stdout", b"first")?;
        store.worker.call(|db| {
            db.db.execute(
                "UPDATE jobs SET log_bytes=?1 WHERE job_id='TEST01'",
                [MAX_JOB_LOG_BYTES],
            )?;
            Ok(())
        })?;
        assert_eq!(store.append("TEST01", "stderr", b"truncated")?, None);
        let finished = store.finish_sync(
            "TEST01",
            outcome(JobState::Succeeded, Some("DETACHED_OUTPUT")),
        )?;
        assert_eq!(finished.last_log_seq, 1);
        assert_eq!(finished.output_loss_reason.as_deref(), Some("TRUNCATED"));
        assert_eq!(finished.output_complete, Some(false));
        let changes = store.subscribe();
        store.finish_sync("TEST01", outcome(JobState::Failed, None))?;
        assert!(!changes.has_changed()?);
        assert_eq!(store.append("TEST01", "stdout", b"late")?, None);
        assert_eq!(store.get("TEST01")?.unwrap().state, JobState::Succeeded);
        assert_eq!(store.get("TEST01")?.unwrap().last_log_seq, 1);
        store.worker.call(|database| {
            database.db.execute(
                "UPDATE jobs SET finished_at_ms=?1 WHERE job_id='TEST01'",
                [now_ms() - FINISHED_LOG_RETENTION_MS - 1],
            )?;
            Ok(())
        })?;
        store.prune()?;
        let late = store.finish_sync("TEST01", outcome(JobState::Failed, None))?;
        assert_eq!(late.output_loss_reason.as_deref(), Some("LOG_EXPIRED"));
        assert_eq!(late.output_complete, Some(false));
        assert_eq!(late.last_log_seq, 1);
        assert!(store.logs("TEST01", 0)?.is_empty());
        Ok(())
    }
    #[test]
    fn failed_append_rolls_back_counters_and_does_not_notify() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = JobStore::open(&dir.path().join("tasks.db"), true)?;
        store.insert(&job(&store))?;
        let changes = store.subscribe();
        store.worker.call(|db| {
            db.db.execute_batch("CREATE TRIGGER reject_log BEFORE INSERT ON job_logs BEGIN SELECT RAISE(FAIL,'injected disk failure'); END;")?;
            Ok(())
        })?;
        assert!(store.append("TEST01", "stdout", b"never visible").is_err());
        assert!(!changes.has_changed()?);
        assert_eq!(store.get("TEST01")?.unwrap().last_log_seq, 0);
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
        let store = JobStore::open(&path, true)?;
        store.insert(&job(&store))?;
        let competing_writer = Connection::open(&path)?;
        competing_writer.busy_timeout(std::time::Duration::ZERO)?;
        let updated = store.worker.call(move |database| {
            database.update("TEST01", |job| {
                // This step runs after the task snapshot was read. An external
                // writer must wait rather than making that snapshot stale.
                let write = competing_writer
                    .execute("UPDATE jobs SET last_log_seq=999 WHERE job_id='TEST01'", []);
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
        assert_eq!(updated.last_log_seq, 0);
        assert_eq!(store.get("TEST01")?.unwrap().last_log_seq, 0);
        Ok(())
    }
    #[tokio::test]
    async fn canceled_reader_writes_are_flushed_before_finish() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = std::sync::Arc::new(JobStore::open(&dir.path().join("tasks.db"), true)?);
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
        assert_eq!(finished.last_log_seq, 1);
        assert_eq!(store.logs("TEST01", 0)?.len(), 1);
        Ok(())
    }
}
