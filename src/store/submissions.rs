use super::*;
const RECENT_SUBMISSION_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
pub(crate) const SUBMISSION_SCHEMA_VERSION: i64 = 2;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct Submission {
    pub request_id: String,
    pub source_device_id: String,
    pub target_device_id: String,
    pub target_name: String,
    pub ca_pin: String,
    pub db_id: String,
    pub request_hash: String,
    pub kind: JobKind,
    pub label: String,
    pub created_at_ms: i64,
    pub job_id: Option<String>,
    pub status: String,
}
pub(crate) struct SubmissionStore(Mutex<Connection>);
impl SubmissionStore {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let mut db = open(path, true)?;
        crate::database::initialize_with_backup(
            &mut db,
            path,
            "submissions",
            SUBMISSION_SCHEMA_VERSION,
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
    pub(crate) fn get(&self, id: &str) -> Result<Option<Submission>> {
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
    pub(crate) fn save(&self, s: &Submission) -> Result<()> {
        self.0.lock().unwrap().execute(
            "INSERT INTO submissions VALUES(?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET data=excluded.data",
            params![s.request_id, serde_json::to_string(s)?, s.created_at_ms],
        )?;
        Ok(())
    }
    pub(crate) fn recent(&self) -> Result<Vec<Submission>> {
        let db = self.0.lock().unwrap();
        let mut s = db.prepare("SELECT data FROM submissions WHERE time>=?1 ORDER BY time DESC")?;
        s.query_map([now_ms() - RECENT_SUBMISSION_WINDOW_MS], |r| {
            r.get::<_, String>(0)
        })?
        .map(|r| decode(r?))
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_open_backs_up_old_submission_payloads_and_accepts_new_operations() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("submissions.sqlite");
        let db = Connection::open(&path)?;
        db.execute_batch(
            "CREATE TABLE submissions(id TEXT PRIMARY KEY,data TEXT NOT NULL,time INTEGER NOT NULL);
             INSERT INTO submissions VALUES('old','old payload without kind or label',0);
             PRAGMA user_version=1;",
        )?;
        drop(db);
        let store = SubmissionStore::open(&path)?;
        assert!(store.recent()?.is_empty());
        let operation = Submission {
            request_id: "new".into(),
            source_device_id: "source".into(),
            target_device_id: "target".into(),
            target_name: "peer".into(),
            ca_pin: "pin".into(),
            db_id: "db_new".into(),
            request_hash: "hash".into(),
            kind: JobKind::Forward,
            label: "forward 8080".into(),
            created_at_ms: now_ms(),
            job_id: None,
            status: "pending".into(),
        };
        store.save(&operation)?;
        drop(store);
        let store = SubmissionStore::open(&path)?;
        assert_eq!(store.recent()?.len(), 1);
        assert_eq!(store.get("new")?.unwrap().label, operation.label);
        let copies = std::fs::read_dir(directory.path().join("database-backups"))?
            .collect::<std::io::Result<Vec<_>>>()?;
        assert_eq!(copies.len(), 1);
        let backup = Connection::open(copies[0].path().join("submissions.sqlite"))?;
        assert_eq!(
            backup.query_row("SELECT data FROM submissions", [], |row| row
                .get::<_, String>(0))?,
            "old payload without kind or label"
        );
        Ok(())
    }

    #[test]
    fn parallel_cli_openers_initialize_the_same_submission_store() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("submissions.sqlite");
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| -> Result<()> {
            let workers: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        SubmissionStore::open(&path).map(|_| ())
                    })
                })
                .collect();
            for worker in workers {
                worker.join().expect("CLI database opener panicked")?;
            }
            Ok(())
        })
    }
}
