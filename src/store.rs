use crate::protocol::{Device, Job, LogEvent};
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::{path::Path, sync::Mutex};

pub struct Store(pub Mutex<Connection>, bool);

impl Store {
    pub fn open(path: &Path, agent: bool) -> Result<Self> {
        if !agent && !path.exists() {
            std::fs::File::create(path)?;
        }
        if agent && !path.exists() {
            anyhow::bail!(
                "agent database is missing; initialize it with xrun agent init before running"
            );
        }
        let db = Connection::open(path)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "FULL")?;
        db.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS devices(id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, cert_fp TEXT NOT NULL UNIQUE, key_fp TEXT NOT NULL, data TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS device_certs(fp TEXT PRIMARY KEY, device_id TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS tokens(hash TEXT PRIMARY KEY, expires INTEGER NOT NULL, renew_id TEXT, used_key_fp TEXT, response TEXT);
            CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY, source TEXT NOT NULL, request_id TEXT NOT NULL, data TEXT NOT NULL, UNIQUE(source, request_id));
            CREATE TABLE IF NOT EXISTS logs(job_id TEXT NOT NULL, seq INTEGER NOT NULL, data TEXT NOT NULL, PRIMARY KEY(job_id,seq));
            CREATE TABLE IF NOT EXISTS process(job_id TEXT PRIMARY KEY, pid INTEGER NOT NULL, boot_id TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS audit(id INTEGER PRIMARY KEY AUTOINCREMENT, at_ms INTEGER NOT NULL, event TEXT NOT NULL, data TEXT NOT NULL);")?;
        db.execute(
            "INSERT OR IGNORE INTO device_certs(fp,device_id) SELECT cert_fp,id FROM devices",
            [],
        )?;
        db.execute("INSERT OR IGNORE INTO meta(key,value) VALUES('log_bytes',(SELECT COALESCE(SUM(length(json_extract(data,'$.data_base64'))*3/4),0) FROM logs))",[])?;
        Ok(Self(Mutex::new(db), agent))
    }

    pub fn init_agent(path: &Path) -> Result<()> {
        if path.exists() {
            anyhow::bail!("agent database already exists");
        }
        let store = Self::open_new_agent(path)?;
        let db = store.0.lock().unwrap();
        db.execute(
            "INSERT INTO meta(key,value) VALUES('store_id',?1)",
            [format!("store_{}", uuid::Uuid::new_v4())],
        )?;
        Ok(())
    }

    fn open_new_agent(path: &Path) -> Result<Self> {
        std::fs::File::create_new(path)?;
        Self::open(path, true)
    }

    pub fn store_id(&self) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
                r.get(0)
            })
            .context("store_id missing")
    }

    pub fn device(&self, id_or_name: &str) -> Result<Option<(Device, bool)>> {
        let db = self.0.lock().unwrap();
        let row: Option<(String, i64)> = db
            .query_row(
                "SELECT data,revoked FROM devices WHERE id=?1 OR name=?1",
                [id_or_name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        row.map(|(s, v)| Ok((serde_json::from_str(&s)?, v != 0)))
            .transpose()
    }

    pub fn device_by_cert(&self, fp: &str) -> Result<Option<(Device, bool)>> {
        let db = self.0.lock().unwrap();
        let row:Option<(String,i64)>=db.query_row("SELECT d.data,d.revoked FROM device_certs c JOIN devices d ON d.id=c.device_id WHERE c.fp=?1",[fp],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        row.map(|(s, v)| Ok((serde_json::from_str(&s)?, v != 0)))
            .transpose()
    }

    pub fn list_devices(&self) -> Result<Vec<Device>> {
        let db = self.0.lock().unwrap();
        let mut st = db.prepare("SELECT data FROM devices WHERE revoked=0 ORDER BY name")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn update_device(&self, d: &Device) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE devices SET data=?2 WHERE id=?1",
            params![d.device_id, serde_json::to_string(d)?],
        )?;
        Ok(())
    }

    pub fn revoke(&self, id: &str) -> Result<bool> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .execute("UPDATE devices SET revoked=1 WHERE id=?1", [id])?
            > 0)
    }

    pub fn insert_job(&self, j: &Job) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT INTO jobs(id,source,request_id,data) VALUES(?1,?2,?3,?4)",
            params![
                j.job_id,
                j.source_device_id,
                j.request_id,
                serde_json::to_string(j)?
            ],
        )?;
        Ok(())
    }

    pub fn save_job(&self, j: &Job) -> Result<()> {
        self.0.lock().unwrap().execute(
            "UPDATE jobs SET data=?2 WHERE id=?1",
            params![j.job_id, serde_json::to_string(j)?],
        )?;
        Ok(())
    }

    pub fn job(&self, id: &str) -> Result<Option<Job>> {
        let db = self.0.lock().unwrap();
        let s: Option<String> = db
            .query_row("SELECT data FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        s.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn job_by_request(&self, source: &str, request_id: &str) -> Result<Option<Job>> {
        let db = self.0.lock().unwrap();
        let s: Option<String> = db
            .query_row(
                "SELECT data FROM jobs WHERE source=?1 AND request_id=?2",
                params![source, request_id],
                |r| r.get(0),
            )
            .optional()?;
        s.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
    }

    pub fn jobs(
        &self,
        source: &str,
        device: Option<&str>,
        request_id: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<Job>> {
        let db = self.0.lock().unwrap();
        let mut st=db.prepare("SELECT data FROM jobs WHERE source=?1 AND (?2 IS NULL OR json_extract(data,'$.target_device_id')=?2) AND (?3 IS NULL OR request_id=?3) ORDER BY rowid DESC LIMIT ?4 OFFSET ?5")?;
        let rows = st.query_map(
            params![source, device, request_id, limit as i64, offset as i64],
            |r| r.get::<_, String>(0),
        )?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn all_jobs(&self) -> Result<Vec<Job>> {
        let db = self.0.lock().unwrap();
        let mut st = db.prepare("SELECT data FROM jobs ORDER BY rowid")?;
        let rows = st.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn save_process(&self, id: &str, pid: u32, boot_id: &str) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT INTO process(job_id,pid,boot_id) VALUES(?1,?2,?3) ON CONFLICT(job_id) DO UPDATE SET pid=excluded.pid,boot_id=excluded.boot_id",
            params![id, pid as i64, boot_id],
        )?;
        Ok(())
    }

    pub fn process(&self, id: &str) -> Result<Option<(u32, String)>> {
        let db = self.0.lock().unwrap();
        let v: Option<(i64, String)> = db
            .query_row(
                "SELECT pid,boot_id FROM process WHERE job_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(v.map(|(pid, boot)| (pid as u32, boot)))
    }

    pub fn append_log(&self, e: &LogEvent) -> Result<bool> {
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM logs WHERE job_id=?1 AND seq=?2)",
            params![e.job_id, e.seq as i64],
            |r| r.get(0),
        )?;
        if exists {
            return Ok(true);
        }
        if self.1 {
            let size =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &e.data_base64)?
                    .len() as i64;
            let mut used: i64 =
                tx.query_row("SELECT value FROM meta WHERE key='log_bytes'", [], |r| {
                    let text: String = r.get(0)?;
                    Ok(text.parse::<i64>().unwrap_or(0))
                })?;
            while used + size > 1024 * 1024 * 1024 {
                let victim:Option<(String,i64)>=tx.query_row("SELECT l.job_id,COALESCE(SUM(length(json_extract(l.data,'$.data_base64'))*3/4),0) FROM logs l JOIN jobs j ON j.id=l.job_id WHERE json_extract(j.data,'$.state') IN ('exited','failed','canceled','timed_out','lost') GROUP BY l.job_id ORDER BY MIN(l.rowid) LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                let Some((victim, bytes)) = victim else {
                    return Ok(false);
                };
                tx.execute("DELETE FROM logs WHERE job_id=?1", [victim])?;
                used = (used - bytes).max(0);
            }
            tx.execute(
                "UPDATE meta SET value=?1 WHERE key='log_bytes'",
                [(used + size).to_string()],
            )?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO logs(job_id,seq,data) VALUES(?1,?2,?3)",
            params![e.job_id, e.seq as i64, serde_json::to_string(e)?],
        )?;
        if !self.1 {
            loop {
                let size:i64=tx.query_row("SELECT COALESCE(SUM(length(json_extract(data,'$.data_base64'))*3/4),0) FROM logs WHERE job_id=?1",[&e.job_id],|r|r.get(0))?;
                if size <= 64 * 1024 {
                    break;
                }
                tx.execute("DELETE FROM logs WHERE rowid=(SELECT rowid FROM logs WHERE job_id=?1 ORDER BY seq LIMIT 1)",[&e.job_id])?;
            }
            loop {
                let size:i64=tx.query_row("SELECT COALESCE(SUM(length(json_extract(data,'$.data_base64'))*3/4),0) FROM logs",[],|r|r.get(0))?;
                if size <= 64 * 1024 * 1024 {
                    break;
                }
                tx.execute(
                    "DELETE FROM logs WHERE rowid=(SELECT rowid FROM logs ORDER BY rowid LIMIT 1)",
                    [],
                )?;
            }
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn prune_old_logs(&self) -> Result<()> {
        let db = self.0.lock().unwrap();
        let cutoff = crate::protocol::now_ms().saturating_sub(7 * 24 * 3600 * 1000) as i64;
        db.execute("DELETE FROM logs WHERE job_id IN (SELECT id FROM jobs WHERE json_extract(data,'$.state') IN ('exited','failed','canceled','timed_out','lost') AND CAST(json_extract(data,'$.updated_at_ms') AS INTEGER) < ?1)",[cutoff])?;
        let size: i64 = db.query_row(
            "SELECT COALESCE(SUM(length(json_extract(data,'$.data_base64'))*3/4),0) FROM logs",
            [],
            |r| r.get(0),
        )?;
        db.execute(
            "UPDATE meta SET value=?1 WHERE key='log_bytes'",
            [size.to_string()],
        )?;
        Ok(())
    }

    pub fn logs(&self, id: &str, after: u64) -> Result<Vec<LogEvent>> {
        let db = self.0.lock().unwrap();
        let mut st =
            db.prepare("SELECT data FROM logs WHERE job_id=?1 AND seq>?2 ORDER BY seq LIMIT 16")?;
        let rows = st.query_map(params![id, after as i64], |r| r.get::<_, String>(0))?;
        rows.map(|r| Ok(serde_json::from_str(&r?)?)).collect()
    }

    pub fn log_range(&self, id: &str) -> Result<(Option<u64>, Option<u64>)> {
        let db = self.0.lock().unwrap();
        let (min, max): (Option<i64>, Option<i64>) = db.query_row(
            "SELECT MIN(seq),MAX(seq) FROM logs WHERE job_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((min.map(|v| v as u64), max.map(|v| v as u64)))
    }

    pub fn token(&self, hash: &str, expires: u64, renew_id: Option<&str>) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT INTO tokens(hash,expires,renew_id) VALUES(?1,?2,?3)",
            params![hash, expires as i64, renew_id],
        )?;
        Ok(())
    }

    pub fn consume_token(
        &self,
        hash: &str,
        response: &str,
        device: &Device,
        cert_fp: &str,
        key_fp: &str,
    ) -> Result<Option<String>> {
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        type TokenRow = (i64, Option<String>, Option<String>, Option<String>);
        let row: Option<TokenRow> = tx
            .query_row(
                "SELECT expires,renew_id,used_key_fp,response FROM tokens WHERE hash=?1",
                [hash],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((expires, renew_id, used_key, old_response)) = row else {
            anyhow::bail!("invalid pairing token");
        };
        if expires < crate::protocol::now_ms() as i64 {
            anyhow::bail!("pairing token expired");
        }
        if let Some(used) = used_key {
            if used == key_fp {
                return Ok(old_response);
            }
            anyhow::bail!("pairing token already consumed by another key");
        }
        if let Some(renew_id) = renew_id {
            if renew_id != device.device_id {
                anyhow::bail!("renewal identity mismatch");
            }
            let old: Option<(String, i64)> = tx
                .query_row(
                    "SELECT key_fp,revoked FROM devices WHERE id=?1",
                    [&renew_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if old != Some((key_fp.to_owned(), 0)) {
                anyhow::bail!("renewal requires same active public key");
            }
            tx.execute(
                "UPDATE devices SET cert_fp=?2 WHERE id=?1",
                params![renew_id, cert_fp],
            )?;
            tx.execute(
                "INSERT INTO device_certs(fp,device_id) VALUES(?1,?2)",
                params![cert_fp, renew_id],
            )?;
        } else {
            tx.execute(
                "INSERT INTO devices(id,name,cert_fp,key_fp,data) VALUES(?1,?2,?3,?4,?5)",
                params![
                    device.device_id,
                    device.name,
                    cert_fp,
                    key_fp,
                    serde_json::to_string(device)?
                ],
            )?;
            tx.execute(
                "INSERT INTO device_certs(fp,device_id) VALUES(?1,?2)",
                params![cert_fp, device.device_id],
            )?;
        }
        tx.execute(
            "UPDATE tokens SET used_key_fp=?2,response=?3 WHERE hash=?1",
            params![hash, key_fp, response],
        )?;
        tx.commit()?;
        Ok(None)
    }

    pub fn token_renew_id(&self, hash: &str) -> Result<Option<String>> {
        self.0
            .lock()
            .unwrap()
            .query_row("SELECT renew_id FROM tokens WHERE hash=?1", [hash], |r| {
                r.get(0)
            })
            .optional()
            .map(|x| x.flatten())
            .map_err(Into::into)
    }

    pub fn renew_cert(&self, id: &str, key_fp: &str, cert_fp: &str) -> Result<()> {
        let mut db = self.0.lock().unwrap();
        let tx = db.transaction()?;
        let changed = tx.execute(
            "UPDATE devices SET cert_fp=?3 WHERE id=?1 AND key_fp=?2 AND revoked=0",
            params![id, key_fp, cert_fp],
        )?;
        if changed != 1 {
            anyhow::bail!("renewal requires same active public key");
        }
        tx.execute(
            "INSERT INTO device_certs(fp,device_id) VALUES(?1,?2)",
            params![cert_fp, id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn audit(&self, event: &str, data: serde_json::Value) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT INTO audit(at_ms,event,data) VALUES(?1,?2,?3)",
            params![crate::protocol::now_ms() as i64, event, data.to_string()],
        )?;
        Ok(())
    }
}
