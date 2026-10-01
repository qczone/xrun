use crate::{config::restrict_dir, protocol::*};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Mutex};

fn open(path: &Path, create: bool) -> Result<Connection> {
    if !create && !path.exists() {
        bail!(
            "DB_MISSING: {} (use daemon reset while stopped)",
            path.display()
        )
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
        bail!("DB_CORRUPT: {integrity}")
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

#[derive(Clone, Serialize, Deserialize)]
pub struct Registered {
    pub device: Device,
    pub key_fp: String,
    pub registration: Registration,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Invitation {
    pub inviter_id: Option<String>,
    pub allow: bool,
    pub admin: bool,
}
pub struct ServerStore(Mutex<Connection>);
impl ServerStore {
    pub fn open(path: &Path) -> Result<Self> {
        let db = open(path, true)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS devices(id TEXT PRIMARY KEY,name TEXT NOT NULL,key_fp TEXT UNIQUE NOT NULL,revoked INTEGER NOT NULL DEFAULT 0,data TEXT NOT NULL); CREATE UNIQUE INDEX IF NOT EXISTS active_name ON devices(name) WHERE revoked=0; CREATE TABLE IF NOT EXISTS tokens(hash TEXT PRIMARY KEY,expires INTEGER NOT NULL,data TEXT NOT NULL); CREATE TABLE IF NOT EXISTS audit(time INTEGER NOT NULL,event TEXT NOT NULL,data TEXT NOT NULL);")?;
        Ok(Self(Mutex::new(db)))
    }
    pub fn get(&self, id: &str) -> Result<Option<Registered>> {
        let db = self.0.lock().unwrap();
        let value: Option<String> = db.query_row("SELECT data FROM devices WHERE id=?1 OR name=?1 ORDER BY id=?1 DESC,revoked ASC,rowid DESC LIMIT 1", [id], |r| r.get(0)).optional()?;
        value.map(decode).transpose()
    }
    pub fn by_key(&self, key: &str) -> Result<Option<Registered>> {
        let db = self.0.lock().unwrap();
        let value: Option<String> = db
            .query_row("SELECT data FROM devices WHERE key_fp=?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        value.map(decode).transpose()
    }
    pub fn list(&self) -> Result<Vec<Device>> {
        let db = self.0.lock().unwrap();
        let mut stmt = db.prepare("SELECT data FROM devices ORDER BY name,id")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(decode::<Registered>(r?)?.device))
            .collect()
    }
    pub fn save(&self, value: &Registered) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE devices SET revoked=?2,data=?3 WHERE id=?1",
            params![
                value.device.device_id,
                value.device.revoked,
                serde_json::to_string(value)?
            ],
        )?;
        Ok(())
    }
    pub fn metadata(
        &self,
        id: &str,
        os: String,
        arch: String,
        hostname: Option<String>,
        user: Option<String>,
        cwd: Option<String>,
    ) -> Result<()> {
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        let data: String =
            tx.query_row("SELECT data FROM devices WHERE id=?1", [id], |r| r.get(0))?;
        let mut value: Registered = decode(data)?;
        if value.device.revoked {
            bail!("DEVICE_REVOKED: identity has been revoked")
        }
        value.device.os = Some(os);
        value.device.arch = Some(arch);
        value.device.version = Some(VERSION.into());
        value.device.hostname = hostname;
        value.device.execution_user = user;
        value.device.default_cwd = cwd;
        value.device.last_seen = Some(now_ms());
        tx.execute(
            "UPDATE devices SET data=?2 WHERE id=?1",
            params![id, serde_json::to_string(&value)?],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn seen(&self, id: &str) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE devices SET data=json_set(data,'$.device.last_seen',?2) WHERE id=?1",
            params![id, now_ms()],
        )?;
        Ok(())
    }
    pub fn invite(&self, invitation: &Invitation) -> Result<String> {
        let token = crate::crypto::random_token();
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        if !inviter_active(&tx, invitation)? {
            bail!("DEVICE_REVOKED: inviter is no longer active")
        }
        tx.execute(
            "INSERT INTO tokens VALUES(?1,?2,?3)",
            params![
                sha256(token.as_bytes()),
                now_ms() + 600_000,
                serde_json::to_string(invitation)?
            ],
        )?;
        tx.commit()?;
        Ok(token)
    }
    pub fn register(&self, token: &str, name: &str, key_fp: &str) -> Result<(Registered, bool)> {
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        let existing: Option<String> = tx
            .query_row("SELECT data FROM devices WHERE key_fp=?1", [key_fp], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(value) = existing {
            let value: Registered = decode(value)?;
            if value.device.revoked {
                bail!("DEVICE_REVOKED: identity has been revoked")
            }
            return Ok((value, false));
        }
        if !valid_name(name) {
            bail!("INVALID_NAME: expected a nonreserved lowercase device name")
        }
        let invitation: String = tx
            .query_row(
                "SELECT data FROM tokens WHERE hash=?1 AND expires>?2",
                params![sha256(token.as_bytes()), now_ms()],
                |r| r.get(0),
            )
            .optional()?
            .context("INVALID_TOKEN: expired or consumed invitation")?;
        let invitation: Invitation = decode(invitation)?;
        if !inviter_active(&tx, &invitation)? {
            bail!("INVALID_TOKEN: inviter is no longer active")
        }
        let value = Registered {
            device: Device {
                device_id: format!("dev_{}", uuid::Uuid::new_v4().simple()),
                name: name.into(),
                online: false,
                admin: invitation.admin,
                revoked: false,
                os: None,
                arch: None,
                version: None,
                hostname: None,
                execution_user: None,
                default_cwd: None,
                last_seen: None,
            },
            key_fp: key_fp.into(),
            registration: Registration {
                inviter_id: invitation.inviter_id,
                allow_inviter: invitation.allow,
            },
        };
        tx.execute(
            "INSERT INTO devices VALUES(?1,?2,?3,0,?4)",
            params![
                value.device.device_id,
                name,
                key_fp,
                serde_json::to_string(&value)?
            ],
        )
        .context("NAME_IN_USE: choose another device name")?;
        tx.execute(
            "DELETE FROM tokens WHERE hash=?1",
            [sha256(token.as_bytes())],
        )?;
        tx.commit()?;
        Ok((value, true))
    }
    pub fn revoke(&self, selector: &str) -> Result<Registered> {
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        let data: String = tx.query_row("SELECT data FROM devices WHERE id=?1 OR name=?1 ORDER BY id=?1 DESC,revoked ASC,rowid DESC LIMIT 1", [selector], |r| r.get(0))
            .optional()?.context("UNKNOWN_DEVICE: device not registered")?;
        let mut target: Registered = decode(data)?;
        if target.device.admin {
            bail!("ADMIN_PROTECTED: administrator identity cannot be revoked")
        }
        target.device.revoked = true;
        tx.execute(
            "UPDATE devices SET revoked=1,data=?2 WHERE id=?1",
            params![target.device.device_id, serde_json::to_string(&target)?],
        )?;
        tx.execute(
            "DELETE FROM tokens WHERE json_extract(data,'$.inviter_id')=?1",
            [&target.device.device_id],
        )?;
        tx.commit()?;
        Ok(target)
    }
    pub fn audit(&self, event: &str, value: serde_json::Value) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT INTO audit VALUES(?1,?2,?3)",
            params![now_ms(), event, serde_json::to_string(&value)?],
        )?;
        Ok(())
    }

    // Only the local deployment command calls this; it requires access to the CA
    // private key. Keep an interrupted bootstrap's identity if its key survived.
    pub fn recover_admin(&self, pending_key: Option<&str>) -> Result<()> {
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        let rows: Vec<String> = {
            let mut stmt = tx.prepare("SELECT data FROM devices WHERE revoked=0")?;
            stmt.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        for row in rows {
            let mut old: Registered = decode(row)?;
            if old.device.admin && pending_key != Some(old.key_fp.as_str()) {
                old.device.revoked = true;
                tx.execute(
                    "UPDATE devices SET revoked=1,data=?2 WHERE id=?1",
                    params![old.device.device_id, serde_json::to_string(&old)?],
                )?;
                tx.execute(
                    "DELETE FROM tokens WHERE json_extract(data,'$.inviter_id')=?1",
                    [&old.device.device_id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

fn inviter_active(db: &Connection, invitation: &Invitation) -> Result<bool> {
    match &invitation.inviter_id {
        Some(id) => Ok(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM devices WHERE id=?1 AND revoked=0)",
            [id],
            |r| r.get(0),
        )?),
        None => Ok(true), // Local CA bootstrap has no inviting device.
    }
}

pub struct TaskStore {
    db: Mutex<Connection>,
    pub db_id: String,
}
impl TaskStore {
    pub fn open(path: &Path, create: bool) -> Result<Self> {
        let db = open(path, create)?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY,value TEXT NOT NULL); CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY,source TEXT NOT NULL,request_id TEXT NOT NULL,data TEXT NOT NULL,UNIQUE(source,request_id)); CREATE TABLE IF NOT EXISTS logs(job TEXT NOT NULL,seq INTEGER NOT NULL,stream TEXT NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(job,seq)); CREATE TABLE IF NOT EXISTS log_sizes(job TEXT PRIMARY KEY,bytes INTEGER NOT NULL); INSERT OR IGNORE INTO log_sizes SELECT job,SUM(length(bytes)) FROM logs GROUP BY job; INSERT OR IGNORE INTO meta VALUES('log_bytes',(SELECT COALESCE(SUM(bytes),0) FROM log_sizes)); CREATE TABLE IF NOT EXISTS audit(time INTEGER NOT NULL,data TEXT NOT NULL);")?;
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
            None => bail!("DB_CORRUPT: missing db_id"),
        };
        Ok(Self {
            db: Mutex::new(db),
            db_id,
        })
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
        Ok(())
    }
    pub fn save(&self, job: &Job) -> Result<()> {
        self.db.lock().unwrap().execute(
            "UPDATE jobs SET data=?2 WHERE id=?1",
            params![job.job_id, serde_json::to_string(job)?],
        )?;
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
