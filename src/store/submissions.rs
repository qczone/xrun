use super::*;
const RECENT_SUBMISSION_WINDOW_MS: i64 = 24 * 60 * 60 * 1000;
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
        crate::database::initialize(
            &mut db,
            "submissions",
            2,
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
