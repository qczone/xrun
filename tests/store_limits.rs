use anyhow::Result;
use rusqlite::{Connection, params};
use xrun::testing::{protocol::*, store::JobStore};

struct Store {
    tasks: JobStore,
    db: Connection,
    _dir: tempfile::TempDir,
}
impl Store {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("tasks.db");
        Ok(Self {
            tasks: JobStore::open(&path, true)?,
            db: Connection::open(path)?,
            _dir: dir,
        })
    }
    fn job(&self, id: &str, state: JobState, updated: i64) -> Result<()> {
        self.tasks.insert(&Job {
            job_id: id.into(),
            request_id: format!("request-{id}"),
            request_hash: "request-hash".into(),
            source_device_id: "source".into(),
            target_device_id: "target".into(),
            db_id: self.tasks.db_id.clone(),
            state,
            last_log_seq: 0,
            output_loss_reason: None,
            created_at_ms: updated,
            updated_at_ms: updated,
            leftover_possible: false,
            process: None,
            details: JobDetails::Exec(CommandParams {
                program: "test-program".into(),
                args: vec![],
                cwd: String::new(),
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
            finished_at_ms: if (state).terminal() {
                Some(updated)
            } else {
                None
            },
        })
    }
    fn account(&self, id: &str, bytes: u64) -> Result<()> {
        // Put the persisted byte counters at the real production boundary.
        // Keep actual log rows small: this test need not write a GiB of padding.
        self.db.execute(
            "UPDATE jobs SET log_bytes=?2 WHERE job_id=?1",
            params![id, i64::try_from(bytes)?],
        )?;
        self.db.execute(
            "UPDATE meta SET value=(SELECT SUM(log_bytes) FROM jobs) WHERE key='log_bytes'",
            [],
        )?;
        Ok(())
    }
    fn total(&self) -> Result<u64> {
        let bytes: i64 = self.db.query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='log_bytes'",
            [],
            |r| r.get(0),
        )?;
        Ok(bytes.try_into()?)
    }
    fn fill(&self, terminal: bool) -> Result<()> {
        for n in 0..16 {
            let id = format!("job-{n}");
            let state = if terminal && n > 0 {
                JobState::Succeeded
            } else {
                JobState::Running
            };
            self.job(&id, JobState::Running, n)?;
            self.tasks.append(&id, "stdout", b"retained log")?;
            self.account(&id, MAX_FILE)?;
            if state.terminal() {
                let mut job = self.tasks.get(&id)?.unwrap();
                job.state = state;
                job.finished_at_ms = Some(n);
                xrun::testing::replace_job_fixture(&self.tasks, &job)?;
            }
        }
        assert_eq!(self.total()?, 1024 * 1024 * 1024);
        Ok(())
    }
}

#[test]
fn per_job_limit_accepts_the_boundary_and_truncates_without_losing_previous_output() -> Result<()> {
    let s = Store::new()?;
    s.job("job", JobState::Running, 1)?;
    s.tasks.append("job", "stdout", b"previous")?;
    s.account("job", MAX_FILE - 2)?;
    assert_eq!(s.tasks.append("job", "stderr", b"ok")?, Some(2));
    assert_eq!(s.total()?, MAX_FILE);
    for _ in 0..2 {
        assert_eq!(s.tasks.append("job", "stdout", b"overflow")?, None);
        let job = s.tasks.get("job")?.unwrap();
        assert_eq!(job.state, JobState::Running);
        assert_eq!(job.last_log_seq, 2);
        assert_eq!(job.output_complete, Some(false));
        assert_eq!(job.output_loss_reason.as_deref(), Some("TRUNCATED"));
        assert_eq!(s.total()?, MAX_FILE);
    }
    let logs = s.tasks.logs("job", 0)?;
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[1].stream, "stderr");
    assert_eq!(logs[1].data_base64, "b2s=");
    assert_eq!(
        s.tasks.by_request("source", "request-job")?.unwrap().job_id,
        "job"
    );
    Ok(())
}

#[test]
fn global_limit_reclaims_only_oldest_finished_logs_and_rolls_back_failed_appends() -> Result<()> {
    let s = Store::new()?;
    s.fill(true)?;
    s.job("new", JobState::Running, 20)?;
    let oldest = serde_json::to_value(s.tasks.get("job-1")?.unwrap())?;
    // Fail after the eviction statements, inside the very same transaction.
    s.db.execute_batch(
        "CREATE TRIGGER fail_log BEFORE INSERT ON job_logs BEGIN SELECT RAISE(ABORT, 'test log write failure'); END;",
    )?;
    assert!(s.tasks.append("new", "stdout", b"new log").is_err());
    assert_eq!(
        serde_json::to_value(s.tasks.get("job-1")?.unwrap())?,
        oldest
    );
    assert_eq!(s.tasks.logs("job-1", 0)?.len(), 1);
    assert_eq!(s.total()?, 1024 * 1024 * 1024);
    assert_eq!(s.tasks.get("new")?.unwrap().last_log_seq, 0);
    s.db.execute_batch("DROP TRIGGER fail_log;")?;

    assert_eq!(s.tasks.append("new", "stdout", b"new log")?, Some(1));
    let evicted = s.tasks.get("job-1")?.unwrap();
    assert_eq!(evicted.state, JobState::Succeeded);
    assert_eq!(evicted.output_complete, Some(false));
    assert_eq!(evicted.output_loss_reason.as_deref(), Some("LOG_EXPIRED"));
    assert!(s.tasks.logs("job-1", 0)?.is_empty());
    assert_eq!(
        s.tasks
            .by_request("source", "request-job-1")?
            .unwrap()
            .job_id,
        "job-1"
    );
    for id in ["job-0", "job-2", "job-15"] {
        assert_eq!(s.tasks.get(id)?.unwrap().output_complete, Some(true));
        assert_eq!(s.tasks.logs(id, 0)?.len(), 1);
    }
    assert_eq!(s.total()?, 1024 * 1024 * 1024 - MAX_FILE + 7);
    Ok(())
}

#[test]
fn global_limit_never_evicts_running_jobs_and_can_resume_after_one_finishes() -> Result<()> {
    let s = Store::new()?;
    s.fill(false)?;
    s.job("new", JobState::Running, 20)?;
    assert_eq!(s.tasks.append("new", "stdout", b"blocked")?, None);
    assert_eq!(s.total()?, 1024 * 1024 * 1024);
    let partial = s.tasks.get("new")?.unwrap();
    assert_eq!(partial.last_log_seq, 0);
    assert_eq!(partial.output_loss_reason.as_deref(), Some("TRUNCATED"));
    for n in 0..16 {
        let id = format!("job-{n}");
        assert_eq!(s.tasks.get(&id)?.unwrap().output_complete, Some(true));
        assert_eq!(s.tasks.logs(&id, 0)?.len(), 1);
    }
    let mut finished = s.tasks.get("job-0")?.unwrap();
    finished.state = JobState::Succeeded;
    finished.finished_at_ms = Some(now_ms());
    xrun::testing::replace_job_fixture(&s.tasks, &finished)?;
    assert_eq!(s.tasks.append("new", "stdout", b"resumed")?, Some(1));
    // Later output must not hide that an earlier chunk was lost.
    assert_eq!(s.tasks.get("new")?.unwrap().output_complete, Some(false));
    assert_eq!(
        s.tasks.get("new")?.unwrap().output_loss_reason,
        partial.output_loss_reason
    );
    assert_eq!(s.total()?, 1024 * 1024 * 1024 - MAX_FILE + 7);
    Ok(())
}
