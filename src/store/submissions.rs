use super::*;
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
        let mut db = open(path, true)?;
        crate::database::initialize(
            &mut db,
            "submissions",
            1,
            "
            CREATE TABLE submissions(id TEXT PRIMARY KEY,data TEXT NOT NULL,time INTEGER NOT NULL);
            CREATE INDEX submission_time ON submissions(time);
        ",
        )?;
        db.execute(
            "DELETE FROM submissions WHERE time<?1",
            [now_ms() - SUBMISSION_RETENTION_MS],
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
        self.0.lock().unwrap().execute(
            "INSERT INTO submissions VALUES(?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![s.request_id, serde_json::to_string(s)?, s.created_at_ms],
        )?;
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
