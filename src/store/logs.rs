//! Log rows, quotas and loss markers commit together before notifying readers.
use super::{tasks::Database, *};

impl Database {
    pub(super) fn logs(&self, id: &str, after: u64) -> Result<Vec<LogEvent>> {
        let Ok(after) = i64::try_from(after) else {
            return Ok(vec![]);
        };
        let mut statement = self.db.prepare(
            "SELECT seq,stream,bytes FROM logs WHERE job=?1 AND seq>?2 ORDER BY seq LIMIT 16",
        )?;
        Ok(statement
            .query_map(params![id, after], |row| {
                Ok(LogEvent {
                    seq: row.get::<_, i64>(0)? as u64,
                    stream: row.get(1)?,
                    data_base64: STANDARD.encode(row.get::<_, Vec<u8>>(2)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }
    pub(super) fn append(&mut self, id: &str, stream: &str, bytes: &[u8]) -> Result<Option<u64>> {
        let transaction = self.db.transaction()?;
        let mut job = LogState::load(&transaction, id)?;
        if job.terminal {
            return Ok(None);
        }
        let own: i64 = transaction
            .query_row("SELECT bytes FROM log_sizes WHERE job=?1", [id], |row| {
                row.get(0)
            })
            .optional()?
            .unwrap_or(0);
        let mut total: i64 = transaction.query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='log_bytes'",
            [],
            |row| row.get(0),
        )?;
        let incoming = i64::try_from(bytes.len())?;
        if own + incoming <= MAX_JOB_LOG_BYTES && total + incoming > MAX_TOTAL_LOG_BYTES {
            let candidates: Vec<String> = {
                let mut statement = transaction.prepare(
                    "SELECT id FROM jobs JOIN log_sizes ON id=job
                    WHERE bytes>0 AND state NOT IN ('starting','running') ORDER BY updated_at_ms",
                )?;
                statement
                    .query_map([], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for expired in candidates {
                total -= expire_logs(&transaction, &expired)?;
                if total + incoming <= MAX_TOTAL_LOG_BYTES {
                    break;
                }
            }
        }
        let sequence =
            if own + incoming > MAX_JOB_LOG_BYTES || total + incoming > MAX_TOTAL_LOG_BYTES {
                merge_incomplete(&mut job.incomplete_reason, Some("TRUNCATED".into()));
                job.output_complete = false;
                None
            } else {
                job.last_seq = job
                    .last_seq
                    .checked_add(1)
                    .context(ErrorCode::StorageError.error("log sequence exhausted"))?;
                transaction.execute(
                    "INSERT INTO logs VALUES(?1,?2,?3,?4)",
                    params![id, i64::try_from(job.last_seq)?, stream, bytes],
                )?;
                transaction.execute(
                    "INSERT INTO log_sizes VALUES(?1,?2)
                ON CONFLICT(job) DO UPDATE SET bytes=bytes+excluded.bytes",
                    params![id, incoming],
                )?;
                total += incoming;
                Some(job.last_seq)
            };
        job.save(&transaction, id)?;
        transaction.execute("UPDATE meta SET value=?1 WHERE key='log_bytes'", [total])?;
        transaction.commit()?;
        self.changes.send_replace(());
        Ok(sequence)
    }
    pub(super) fn prune(&mut self) -> Result<()> {
        let transaction = self.db.transaction()?;
        transaction.execute(
            "DELETE FROM audit WHERE time<?1",
            [now_ms() - AUDIT_RETENTION_MS],
        )?;
        let expired: Vec<String> = {
            let mut statement = transaction.prepare(
                "SELECT id FROM jobs JOIN log_sizes ON id=job
                WHERE bytes>0 AND state NOT IN ('starting','running') AND updated_at_ms<?1",
            )?;
            statement
                .query_map([now_ms() - FINISHED_LOG_RETENTION_MS], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for id in expired {
            expire_logs(&transaction, &id)?;
        }
        transaction.execute("UPDATE meta SET value=(SELECT COALESCE(SUM(bytes),0) FROM log_sizes) WHERE key='log_bytes'", [])?;
        transaction.commit()?;
        self.changes.send_replace(());
        Ok(())
    }
    pub(super) fn audit(&self, value: serde_json::Value) -> Result<()> {
        self.db.execute(
            "INSERT INTO audit VALUES(?1,?2)",
            params![now_ms(), serde_json::to_string(&value)?],
        )?;
        Ok(())
    }
}
fn expire_logs(db: &Connection, id: &str) -> Result<i64> {
    let bytes = db.query_row("SELECT bytes FROM log_sizes WHERE job=?1", [id], |row| {
        row.get(0)
    })?;
    db.execute("DELETE FROM logs WHERE job=?1", [id])?;
    db.execute("DELETE FROM log_sizes WHERE job=?1", [id])?;
    db.execute(
        "UPDATE jobs SET output_complete=0,incomplete_reason='LOG_EXPIRED' WHERE id=?1",
        [id],
    )?;
    Ok(bytes)
}

struct LogState {
    terminal: bool,
    last_seq: u64,
    output_complete: bool,
    incomplete_reason: Option<String>,
}
impl LogState {
    fn load(db: &Connection, id: &str) -> Result<Self> {
        db.query_row(
            "SELECT state NOT IN ('starting','running'),last_seq,output_complete,incomplete_reason
             FROM jobs WHERE id=?1",
            [id],
            |row| {
                Ok(Self {
                    terminal: row.get(0)?,
                    last_seq: row.get::<_, i64>(1)? as u64,
                    output_complete: row.get(2)?,
                    incomplete_reason: row.get(3)?,
                })
            },
        )
        .optional()?
        .context(ErrorCode::JobNotFound.error("unknown log task"))
    }
    fn save(&self, db: &Connection, id: &str) -> Result<()> {
        db.execute(
            "UPDATE jobs SET last_seq=?2,output_complete=?3,incomplete_reason=?4 WHERE id=?1",
            params![
                id,
                i64::try_from(self.last_seq)?,
                self.output_complete,
                self.incomplete_reason
            ],
        )?;
        Ok(())
    }
}
