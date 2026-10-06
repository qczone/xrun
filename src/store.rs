use crate::error::ErrorCode;
use crate::{config::restrict_dir, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Mutex};

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
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "synchronous", "FULL")?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
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

pub struct TaskStore {
    db: Mutex<Connection>,
    pub db_id: String,
    changes: tokio::sync::watch::Sender<()>,
}
impl TaskStore {
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        let db = open(path, create)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY,source TEXT NOT NULL,request_id TEXT NOT NULL,data TEXT NOT NULL,UNIQUE(source,request_id)); CREATE TABLE IF NOT EXISTS logs(job TEXT NOT NULL,seq INTEGER NOT NULL,stream TEXT NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(job,seq)); CREATE TABLE IF NOT EXISTS log_sizes(job TEXT PRIMARY KEY,bytes INTEGER NOT NULL); INSERT OR IGNORE INTO log_sizes SELECT job,SUM(length(bytes)) FROM logs GROUP BY job; INSERT OR IGNORE INTO meta VALUES('log_bytes',(SELECT COALESCE(SUM(bytes),0) FROM log_sizes)); CREATE TABLE IF NOT EXISTS audit(time INTEGER NOT NULL,data TEXT NOT NULL);")?;
        db.execute_batch("CREATE INDEX IF NOT EXISTS jobs_active ON jobs(source) WHERE json_extract(data,'$.state') IN ('starting','running'); CREATE INDEX IF NOT EXISTS jobs_source ON jobs(source);")?;
        let db_id: Option<String> = db
            .query_row("SELECT value FROM meta WHERE key='db_id'", [], |r| r.get(0))
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
            db: Mutex::new(db),
            db_id,
            changes: tokio::sync::watch::channel(()).0,
        })
    }
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<()> {
        self.changes.subscribe()
    }
    pub fn get(&self, id: &str) -> Result<Option<Job>> {
        let value: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row("SELECT data FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        value.map(decode).transpose()
    }
    pub fn by_request(&self, source: &str, id: &str) -> Result<Option<Job>> {
        let value: Option<String> = self
            .db
            .lock()
            .unwrap()
            .query_row(
                "SELECT data FROM jobs WHERE source=?1 AND request_id=?2",
                params![source, id],
                |r| r.get(0),
            )
            .optional()?;
        value.map(decode).transpose()
    }
    pub fn all(&self) -> Result<Vec<Job>> {
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare("SELECT data FROM jobs ORDER BY rowid DESC")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| decode(r?))
            .collect()
    }
    pub fn active_count(&self) -> Result<usize> {
        let count: i64 = self.db.lock().unwrap().query_row("SELECT COUNT(*) FROM jobs WHERE json_extract(data,'$.state') IN ('starting','running')", [], |r|r.get(0))?;
        Ok(count.try_into()?)
    }
    pub fn page(
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
                .filter(|j| !running || !j.state.terminal())
                .skip(offset)
                .take(limit.min(1000))
                .collect());
        }
        let sql = if running {
            "SELECT data FROM jobs WHERE source=?1 AND json_extract(data,'$.state') IN ('starting','running') ORDER BY rowid DESC LIMIT ?2 OFFSET ?3"
        } else {
            "SELECT data FROM jobs WHERE source=?1 ORDER BY rowid DESC LIMIT ?2 OFFSET ?3"
        };
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(sql)?;
        stmt.query_map(
            params![
                source,
                limit.min(1000) as i64,
                i64::try_from(offset).unwrap_or(i64::MAX)
            ],
            |r| r.get::<_, String>(0),
        )?
        .map(|r| decode(r?))
        .collect()
    }
    pub fn insert(&self, job: &Job) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO jobs VALUES(?1,?2,?3,?4)",
            params![
                job.job_id,
                job.source_device_id,
                job.request_id,
                serde_json::to_string(job)?
            ],
        )?;
        self.changes.send_replace(());
        Ok(())
    }
    pub fn save(&self, job: &Job) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE jobs SET data=?2 WHERE id=?1",
            params![job.job_id, serde_json::to_string(job)?],
        )?;
        self.changes.send_replace(());
        Ok(())
    }
    pub fn logs(&self, job: &str, after: u64) -> Result<Vec<LogEvent>> {
        if after > i64::MAX as u64 {
            return Ok(vec![]);
        }
        let db = self.db.lock().unwrap();
        let mut stmt = db.prepare(
            "SELECT seq,stream,bytes FROM logs WHERE job=?1 AND seq>?2 ORDER BY seq LIMIT 16",
        )?;
        Ok(stmt
            .query_map(params![job, after as i64], |r| {
                Ok(LogEvent {
                    seq: r.get::<_, i64>(0)? as u64,
                    stream: r.get(1)?,
                    data_base64: STANDARD.encode(r.get::<_, Vec<u8>>(2)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn append(&self, job: &str, stream: &str, bytes: &[u8]) -> Result<Option<u64>> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        let own: i64 = tx
            .query_row("SELECT bytes FROM log_sizes WHERE job=?1", [job], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        let mut total: i64 = tx.query_row(
            "SELECT CAST(value AS INTEGER) FROM meta WHERE key='log_bytes'",
            [],
            |r| r.get(0),
        )?;
        if own + bytes.len() as i64 > MAX_FILE as i64 {
            let data: String =
                tx.query_row("SELECT data FROM jobs WHERE id=?1", [job], |r| r.get(0))?;
            let mut value: Job = decode(data)?;
            if value.output_complete {
                value.output_complete = false;
                value.incomplete_reason = Some("TRUNCATED".into());
                tx.execute(
                    "UPDATE jobs SET data=?2 WHERE id=?1",
                    params![job, serde_json::to_string(&value)?],
                )?;
                tx.commit()?;
                self.changes.send_replace(());
            }
            return Ok(None);
        }
        if total + bytes.len() as i64 > 1024 * 1024 * 1024 {
            let candidates: Vec<(String, String, i64)> = {
                let mut s=tx.prepare("SELECT jobs.id,jobs.data,log_sizes.bytes FROM jobs JOIN log_sizes ON jobs.id=log_sizes.job WHERE log_sizes.bytes>0 ORDER BY json_extract(jobs.data,'$.updated_at_ms')")?;
                s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for (id, data, size) in candidates {
                let mut old: Job = decode(data)?;
                if old.state.terminal() {
                    tx.execute("DELETE FROM logs WHERE job=?1", [&id])?;
                    tx.execute("DELETE FROM log_sizes WHERE job=?1", [&id])?;
                    old.output_complete = false;
                    old.incomplete_reason = Some("LOG_EXPIRED".into());
                    tx.execute(
                        "UPDATE jobs SET data=?2 WHERE id=?1",
                        params![id, serde_json::to_string(&old)?],
                    )?;
                    total -= size;
                    if total + bytes.len() as i64 <= 1024 * 1024 * 1024 {
                        break;
                    }
                }
            }
        }
        tx.execute("UPDATE meta SET value=?1 WHERE key='log_bytes'", [total])?;
        if total + bytes.len() as i64 > 1024 * 1024 * 1024 {
            let data: String =
                tx.query_row("SELECT data FROM jobs WHERE id=?1", [job], |r| r.get(0))?;
            let mut value: Job = decode(data)?;
            value.output_complete = false;
            value.incomplete_reason = Some("TRUNCATED".into());
            tx.execute(
                "UPDATE jobs SET data=?2 WHERE id=?1",
                params![job, serde_json::to_string(&value)?],
            )?;
            tx.commit()?;
            self.changes.send_replace(());
            return Ok(None);
        }
        let value: String =
            tx.query_row("SELECT data FROM jobs WHERE id=?1", [job], |r| r.get(0))?;
        let mut value: Job = decode(value)?;
        value.last_seq += 1;
        tx.execute(
            "INSERT INTO logs VALUES(?1,?2,?3,?4)",
            params![job, value.last_seq as i64, stream, bytes],
        )?;
        tx.execute(
            "UPDATE jobs SET data=?2 WHERE id=?1",
            params![job, serde_json::to_string(&value)?],
        )?;
        tx.execute("INSERT INTO log_sizes VALUES(?1,?2) ON CONFLICT(job) DO UPDATE SET bytes=bytes+excluded.bytes",params![job,bytes.len() as i64])?;
        tx.execute(
            "UPDATE meta SET value=?1 WHERE key='log_bytes'",
            [total + bytes.len() as i64],
        )?;
        tx.commit()?;
        self.changes.send_replace(());
        Ok(Some(value.last_seq))
    }
    pub fn prune(&self) -> Result<()> {
        let mut db = self.db.lock().unwrap();
        let tx = db.transaction()?;
        tx.execute(
            "DELETE FROM audit WHERE time<?1",
            [now_ms() - 7 * 86_400_000],
        )?;
        let values: Vec<(String, String)> = {
            let mut s = tx.prepare("SELECT jobs.id,jobs.data FROM jobs JOIN log_sizes ON jobs.id=log_sizes.job WHERE log_sizes.bytes>0")?;
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?
        };
        for (id, value) in values {
            let mut job: Job = decode(value)?;
            if job.state.terminal() && job.updated_at_ms < now_ms() - 7 * 86_400_000 {
                tx.execute("DELETE FROM logs WHERE job=?1", [&id])?;
                tx.execute("DELETE FROM log_sizes WHERE job=?1", [&id])?;
                job.output_complete = false;
                job.incomplete_reason = Some("LOG_EXPIRED".into());
                tx.execute(
                    "UPDATE jobs SET data=?2 WHERE id=?1",
                    params![id, serde_json::to_string(&job)?],
                )?;
            }
        }
        tx.execute("UPDATE meta SET value=(SELECT COALESCE(SUM(bytes),0) FROM log_sizes) WHERE key='log_bytes'",[])?;
        tx.commit()?;
        self.changes.send_replace(());
        Ok(())
    }
    pub fn audit(&self, value: serde_json::Value) -> Result<()> {
        self.db.lock().unwrap().execute(
            "INSERT INTO audit VALUES(?1,?2)",
            params![now_ms(), serde_json::to_string(&value)?],
        )?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    pub request_id: String,
    pub source_device_id: String,
    pub target_device_id: String,
    #[serde(default)]
    pub target_name: String,
    pub ca_pin: String,
    pub db_id: String,
    pub request_hash: String,
    pub program: String,
    pub created_at_ms: i64,
    pub job_id: Option<String>,
    pub status: String,
}
pub struct SubmissionStore(Mutex<Connection>);
impl SubmissionStore {
    pub fn open(path: &Path) -> Result<Self> {
        let db = open(path, true)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS submissions(id TEXT PRIMARY KEY,data TEXT NOT NULL,time INTEGER NOT NULL)")?;
        db.execute_batch("CREATE INDEX IF NOT EXISTS submission_time ON submissions(time)")?;
        db.execute(
            "DELETE FROM submissions WHERE time<?1",
            [now_ms() - 7 * 86_400_000],
        )?;
        Ok(Self(Mutex::new(db)))
    }
    pub fn get(&self, id: &str) -> Result<Option<Submission>> {
        let v: Option<String> = self
            .0
            .lock()
            .unwrap()
            .query_row("SELECT data FROM submissions WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        v.map(decode).transpose()
    }
    pub fn save(&self, s: &Submission) -> Result<()> {
        self.0.lock().unwrap().execute("INSERT INTO submissions VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET data=excluded.data",params![s.request_id,serde_json::to_string(s)?,s.created_at_ms])?;
        Ok(())
    }
    pub fn recent(&self) -> Result<Vec<Submission>> {
        let db = self.0.lock().unwrap();
        let mut s = db.prepare("SELECT data FROM submissions WHERE time>=?1 ORDER BY time DESC")?;
        s.query_map([now_ms() - 86_400_000], |r| r.get::<_, String>(0))?
            .map(|r| decode(r?))
            .collect()
    }
}
