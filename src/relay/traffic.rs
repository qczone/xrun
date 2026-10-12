//! Bounded in-memory counters; one independent writer checkpoints every ten seconds.
use super::*;
use crate::protocol::{
    DeviceTraffic, TrafficBytes, TrafficDay, TrafficPeriod, TrafficQuery, TrafficReport,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::{
    Condvar, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

const DAY_MS: i64 = 86_400_000;
const MAX_PENDING: usize = 4096;
const FLUSH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);
const SCHEMA: &str = "
CREATE TABLE traffic_usage_daily (
    network_id TEXT NOT NULL, device_id TEXT NOT NULL, day_ms INTEGER NOT NULL,
    ingress_bytes INTEGER NOT NULL CHECK(ingress_bytes >= 0),
    egress_bytes INTEGER NOT NULL CHECK(egress_bytes >= 0), first_seen_ms INTEGER NOT NULL,
    PRIMARY KEY(network_id, device_id, day_ms)
) STRICT;
CREATE TABLE traffic_metadata (
    id INTEGER PRIMARY KEY CHECK(id=1), incomplete INTEGER NOT NULL, active_sessions INTEGER NOT NULL
);
INSERT INTO traffic_metadata VALUES(1, 0, 0);
";
const ADD: &str = "INSERT INTO traffic_usage_daily VALUES(?1,?2,?3,?4,?5,?6)
ON CONFLICT(network_id,device_id,day_ms) DO UPDATE SET
ingress_bytes=ingress_bytes+excluded.ingress_bytes,
egress_bytes=egress_bytes+excluded.egress_bytes,
first_seen_ms=MIN(first_seen_ms,excluded.first_seen_ms)";
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    network: Arc<str>,
    device: Arc<str>,
    day: i64,
}
#[derive(Clone, Default)]
struct Delta {
    ingress: i64,
    egress: i64,
    first: i64,
}
impl Delta {
    fn add(&mut self, other: &Self) -> Result<()> {
        let ingress = self
            .ingress
            .checked_add(other.ingress)
            .context("traffic counter overflow")?;
        let egress = self
            .egress
            .checked_add(other.egress)
            .context("traffic counter overflow")?;
        self.ingress = ingress;
        self.egress = egress;
        self.first = if self.first == 0 {
            other.first
        } else {
            self.first.min(other.first)
        };
        Ok(())
    }
}
struct Writer {
    db: std::result::Result<StdMutex<Connection>, String>,
    pending: StdMutex<HashMap<Key, Delta>>,
    flushing: StdMutex<()>,
    incomplete: AtomicBool,
    ended: AtomicU64,
    stop: AtomicBool,
    wake: (StdMutex<bool>, Condvar),
    aggregate: Arc<str>,
}
impl Writer {
    fn fail(&self, error: impl std::fmt::Display) {
        if !self.incomplete.swap(true, Ordering::Relaxed) {
            tracing::warn!(%error, "relay counters incomplete; forwarding remains enabled");
        }
    }
    fn wake(&self) {
        *self.wake.0.lock().unwrap() = true;
        self.wake.1.notify_one();
    }
    fn record(
        &self,
        network: &Arc<str>,
        device: &Arc<str>,
        bytes: usize,
        outgoing: bool,
        now: i64,
    ) -> Result<()> {
        if bytes == 0 {
            return Ok(());
        }
        let bytes = i64::try_from(bytes)?;
        let delta = Delta {
            ingress: if outgoing { 0 } else { bytes },
            egress: if outgoing { bytes } else { 0 },
            first: now,
        };
        let mut pending = self.pending.lock().unwrap();
        for device in std::iter::once(self.aggregate.clone())
            .chain((!device.is_empty()).then(|| device.clone()))
        {
            let key = Key {
                network: network.clone(),
                device,
                day: now.div_euclid(DAY_MS) * DAY_MS,
            };
            if pending.len() >= MAX_PENDING && !pending.contains_key(&key) {
                bail!("traffic pending-key budget exhausted");
            }
            pending.entry(key).or_default().add(&delta)?;
        }
        Ok(())
    }
    fn flush(&self) -> Result<()> {
        let _serial = self.flushing.lock().unwrap();
        self.flush_locked()
    }
    fn flush_locked(&self) -> Result<()> {
        let db = self
            .db
            .as_ref()
            .map_err(|error| ErrorCode::StorageError.error(error))?;
        let batch = std::mem::take(&mut *self.pending.lock().unwrap());
        let ended = self.ended.swap(0, Ordering::AcqRel);
        if batch.is_empty() && ended == 0 && !self.incomplete.load(Ordering::Relaxed) {
            return Ok(());
        }
        let result = (|| -> Result<()> {
            let mut db = db.lock().unwrap();
            let tx = db.transaction()?;
            if self.incomplete.load(Ordering::Relaxed) {
                tx.execute("UPDATE traffic_metadata SET incomplete=1 WHERE id=1", [])?;
            }
            for (key, delta) in &batch {
                tx.prepare_cached(ADD)?.execute(params![
                    key.network.as_ref(),
                    key.device.as_ref(),
                    key.day,
                    delta.ingress,
                    delta.egress,
                    delta.first
                ])?;
            }
            if ended > 0 {
                tx.execute("UPDATE traffic_metadata SET active_sessions=MAX(0,active_sessions-?1) WHERE id=1", [i64::try_from(ended)?])?;
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(error) = result {
            self.ended.fetch_add(ended, Ordering::Relaxed);
            self.fail(&error);
            let mut pending = self.pending.lock().unwrap();
            for (key, delta) in batch {
                if (pending.len() < MAX_PENDING || pending.contains_key(&key))
                    && let Err(error) = pending.entry(key).or_default().add(&delta)
                {
                    self.fail(error);
                }
            }
            drop(pending);
            // Best effort. A full disk may prevent this marker until recovery.
            let _ = db
                .lock()
                .unwrap()
                .execute("UPDATE traffic_metadata SET incomplete=1 WHERE id=1", []);
            return Err(error);
        }
        Ok(())
    }
    fn run(&self) {
        loop {
            let notified = self.wake.0.lock().unwrap();
            let (mut notified, _) = self
                .wake
                .1
                .wait_timeout_while(notified, FLUSH_INTERVAL, |wake| {
                    !*wake && !self.stop.load(Ordering::Acquire)
                })
                .unwrap();
            *notified = false;
            drop(notified);
            if let Err(error) = self.flush() {
                self.fail(error);
            }
            if self.stop.load(Ordering::Acquire) {
                break;
            }
        }
    }
}
/// A session's cached immutable keys avoid per-frame string allocation.
pub(super) struct Counter {
    writer: Arc<Writer>,
    network: Arc<str>,
    device: Arc<str>,
}
impl Counter {
    pub(super) fn record(&self, bytes: usize, outgoing: bool) {
        if let Err(error) =
            self.writer
                .record(&self.network, &self.device, bytes, outgoing, now_ms())
        {
            self.writer.fail(error);
        }
    }
}
pub(super) struct SessionGuard {
    writer: Arc<Writer>,
    active: bool,
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        if self.active {
            self.writer.ended.fetch_add(1, Ordering::Relaxed);
            self.writer.wake();
        }
    }
}
pub(super) struct Traffic {
    writer: Arc<Writer>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Traffic {
    fn drop(&mut self) {
        self.writer.stop.store(true, Ordering::Release);
        self.writer.wake();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        } else if let Err(error) = self.writer.flush() {
            self.writer.fail(error);
        }
    }
}
impl Traffic {
    pub(super) fn open(path: &std::path::Path) -> Self {
        let result = (|| -> Result<Connection> {
            let mut db = Connection::open(path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
            }
            crate::database::configure(&db)?;
            // Only statistical counters permit bounded crash loss. Jobs retain FULL.
            db.pragma_update(None, "synchronous", "NORMAL")?;
            let version: i64 = db.pragma_query_value(None, "user_version", |row| row.get(0))?;
            if version == 1 {
                let tx = db.transaction()?;
                tx.execute_batch("ALTER TABLE traffic_metadata ADD COLUMN active_sessions INTEGER NOT NULL DEFAULT 0; PRAGMA user_version=2;")?;
                tx.commit()?;
            }
            crate::database::initialize(&mut db, "relay traffic", 2, SCHEMA)?;
            db.execute("UPDATE traffic_metadata SET incomplete=MAX(incomplete,active_sessions>0),active_sessions=0 WHERE id=1", [])?;
            Ok(db)
        })();
        Self::from_database(result, true)
    }
    fn from_database(result: Result<Connection>, persistent: bool) -> Self {
        if let Err(error) = &result {
            tracing::warn!(%error, "relay traffic unavailable; forwarding remains enabled");
        }
        let incomplete = result.is_err();
        let writer = Arc::new(Writer {
            db: result.map(StdMutex::new).map_err(|error| error.to_string()),
            pending: StdMutex::new(HashMap::new()),
            flushing: StdMutex::new(()),
            incomplete: AtomicBool::new(incomplete),
            ended: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            wake: (StdMutex::new(false), Condvar::new()),
            aggregate: Arc::from(""),
        });
        let worker = if persistent && writer.db.is_ok() {
            let state = writer.clone();
            match std::thread::Builder::new()
                .name("xrun-traffic".into())
                .spawn(move || state.run())
            {
                Ok(worker) => Some(worker),
                Err(error) => {
                    writer.fail(error);
                    None
                }
            }
        } else {
            None
        };
        Self { writer, worker }
    }
    #[cfg(test)]
    pub(super) fn memory() -> Self {
        Self::from_database(
            (|| {
                let mut db = Connection::open_in_memory()?;
                crate::database::initialize(&mut db, "relay traffic", 2, SCHEMA)?;
                Ok(db)
            })(),
            false,
        )
    }
    pub(super) fn counter(&self, network: &str, device: Option<&str>) -> Counter {
        Counter {
            writer: self.writer.clone(),
            network: network.into(),
            device: device.unwrap_or("").into(),
        }
    }
    pub(super) async fn session(&self) -> SessionGuard {
        let writer = self.writer.clone();
        match tokio::task::spawn_blocking(move || {
            let active = (|| -> Result<()> {
                writer
                    .db
                    .as_ref()
                    .map_err(|error| ErrorCode::StorageError.error(error))?
                    .lock()
                    .unwrap()
                    .execute(
                        "UPDATE traffic_metadata SET active_sessions=active_sessions+1 WHERE id=1",
                        [],
                    )?;
                Ok(())
            })();
            if let Err(error) = &active {
                writer.fail(error);
            }
            SessionGuard {
                writer,
                active: active.is_ok(),
            }
        })
        .await
        {
            Ok(guard) => guard,
            Err(error) => {
                self.writer.fail(error);
                SessionGuard {
                    writer: self.writer.clone(),
                    active: false,
                }
            }
        }
    }
    pub(super) fn record(&self, network: &str, device: Option<&str>, bytes: usize, outgoing: bool) {
        self.counter(network, device).record(bytes, outgoing);
    }
    #[cfg(test)]
    fn record_at(
        &self,
        network: &str,
        device: Option<&str>,
        bytes: usize,
        outgoing: bool,
        now: i64,
    ) -> Result<()> {
        self.writer.record(
            &Arc::from(network),
            &Arc::from(device.unwrap_or("")),
            bytes,
            outgoing,
            now,
        )
    }
    pub(super) fn report(
        &self,
        network: &str,
        device: Option<&str>,
        query: &TrafficQuery,
    ) -> Result<TrafficReport> {
        self.report_at(network, device, query, now_ms())
    }
    fn report_at(
        &self,
        network: &str,
        device: Option<&str>,
        query: &TrafficQuery,
        now: i64,
    ) -> Result<TrafficReport> {
        query.validate()?;
        let _serial = self.writer.flushing.lock().unwrap();
        let _ = self.writer.flush_locked();
        let pending = self.writer.pending.lock().unwrap().clone();
        let db = self
            .writer
            .db
            .as_ref()
            .map_err(|error| anyhow::anyhow!(ErrorCode::StorageError.error(error)))?;
        let mut db = db.lock().map_err(|_| {
            anyhow::anyhow!(ErrorCode::StorageError.error("traffic storage lock failed"))
        })?;
        let transaction = db.transaction()?;
        let start = start_ms(query.period, now)?;
        let key = device.unwrap_or("");
        let mut totals = transaction.query_row(
            "SELECT COALESCE(SUM(ingress_bytes),0),COALESCE(SUM(egress_bytes),0)
             FROM traffic_usage_daily WHERE network_id=?1 AND device_id=?2 AND day_ms>=?3",
            params![network, key, start],
            |row| {
                Ok(TrafficBytes {
                    ingress_bytes: row.get::<_, i64>(0)? as u64,
                    egress_bytes: row.get::<_, i64>(1)? as u64,
                })
            },
        )?;
        let mut recorded_since_ms: Option<i64> = transaction.query_row(
            "SELECT MIN(first_seen_ms) FROM traffic_usage_daily WHERE network_id=?1 AND device_id=?2",
            params![network, key], |row| row.get(0),
        )?;
        let complete = !self.writer.incomplete.load(Ordering::Relaxed)
            && transaction.query_row(
                "SELECT incomplete=0 FROM traffic_metadata WHERE id=1",
                [],
                |row| row.get::<_, bool>(0),
            )?;
        let mut statement = transaction.prepare(
            "SELECT device_id,SUM(ingress_bytes),SUM(egress_bytes) FROM traffic_usage_daily
             WHERE network_id=?1 AND device_id<>'' AND (?2 IS NULL OR device_id=?2) AND day_ms>=?3
             GROUP BY device_id ORDER BY device_id",
        )?;
        let mut devices = statement
            .query_map(params![network, device, start], |row| {
                Ok(DeviceTraffic {
                    device_id: row.get(0)?,
                    sent_bytes: row.get::<_, i64>(1)? as u64,
                    received_bytes: row.get::<_, i64>(2)? as u64,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let today = now.div_euclid(DAY_MS) * DAY_MS;
        let history_start = if query.period == TrafficPeriod::All {
            today - 29 * DAY_MS
        } else {
            start
        };
        let mut daily = Vec::new();
        let mut day = history_start;
        while day <= today {
            let bytes = transaction.query_row(
                "SELECT ingress_bytes,egress_bytes FROM traffic_usage_daily WHERE network_id=?1 AND device_id=?2 AND day_ms=?3",
                params![network, key, day],
                |row| Ok(TrafficBytes { ingress_bytes: row.get::<_, i64>(0)? as u64, egress_bytes: row.get::<_, i64>(1)? as u64 }),
            ).optional()?.unwrap_or_default();
            daily.push(TrafficDay {
                start_ms: day,
                bytes,
            });
            day += DAY_MS;
        }
        for (entry, delta) in pending
            .iter()
            .filter(|(entry, _)| entry.network.as_ref() == network)
        {
            if entry.device.as_ref() == key {
                recorded_since_ms =
                    Some(recorded_since_ms.map_or(delta.first, |first| first.min(delta.first)));
                if entry.day >= start {
                    totals.ingress_bytes += delta.ingress as u64;
                    totals.egress_bytes += delta.egress as u64;
                }
                if let Some(day) = daily.iter_mut().find(|day| day.start_ms == entry.day) {
                    day.bytes.ingress_bytes += delta.ingress as u64;
                    day.bytes.egress_bytes += delta.egress as u64;
                }
            }
            if !entry.device.is_empty()
                && entry.day >= start
                && device.is_none_or(|device| device == entry.device.as_ref())
            {
                if let Some(row) = devices
                    .iter_mut()
                    .find(|row| row.device_id == entry.device.as_ref())
                {
                    row.sent_bytes += delta.ingress as u64;
                    row.received_bytes += delta.egress as u64;
                } else {
                    devices.push(DeviceTraffic {
                        device_id: entry.device.to_string(),
                        sent_bytes: delta.ingress as u64,
                        received_bytes: delta.egress as u64,
                    });
                }
            }
        }
        devices.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        let skipped = (query.offset as usize).min(devices.len());
        devices.drain(..skipped);
        let next_offset = if devices.len() > query.limit as usize {
            devices.truncate(query.limit as usize);
            query.offset.checked_add(query.limit as u32)
        } else {
            None
        };
        Ok(TrafficReport {
            network_id: network.into(),
            device_id: device.map(str::to_owned),
            period: query.period,
            start_ms: start,
            end_ms: now,
            recorded_since_ms,
            complete,
            totals,
            devices,
            daily,
            next_offset,
        })
    }
}
fn start_ms(period: TrafficPeriod, now: i64) -> Result<i64> {
    let day = now.div_euclid(DAY_MS) * DAY_MS;
    Ok(match period {
        TrafficPeriod::Today => day,
        TrafficPeriod::All => 0,
        TrafficPeriod::Month => {
            let date = time::OffsetDateTime::from_unix_timestamp(now / 1000)?.date();
            let first = date.replace_day(1)?.midnight().assume_utc();
            first.unix_timestamp() * 1000
        }
    })
}

pub(super) async fn traffic_route(
    State(app): State<Arc<App>>,
    Path(network): Path<String>,
    Extension(permit): Extension<TransportPermit>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    let selected = protocol(&headers)?;
    app.admission.check(&network, false)?;
    Ok(negotiated(
        ws.max_message_size(MAX_MESSAGE)
            .max_frame_size(MAX_MESSAGE)
            .on_upgrade(move |mut ws| async move {
                let result = async {
                    let path = format!("/networks/{network}/traffic");
                    let (device, manager) = member(&app, &mut ws, &network, &path, &permit).await?;
                    let manager = app.admission.manager(&device, manager)?;
                    send(&mut ws, &RelayMessage::TrafficReady).await?;
                    let RelayMessage::TrafficQuery { query } =
                        tokio::time::timeout(AUTH_TIMEOUT, receive(&mut ws)).await??
                    else {
                        bail!(ErrorCode::InvalidMessage.error("expected a traffic query"));
                    };
                    query.validate()?;
                    let traffic = app.traffic.clone();
                    let device = (!manager).then_some(device);
                    let report = tokio::task::spawn_blocking(move || {
                        traffic.report(&network, device.as_deref(), &query)
                    })
                    .await?
                    .map_err(|error| {
                        anyhow::anyhow!(ErrorCode::StorageError.error(error.to_string()))
                    })?;
                    send(&mut ws, &RelayMessage::Traffic { report }).await
                }
                .await;
                finish(&mut ws, result).await;
            }),
        selected,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn hot_frames_only_update_memory_and_clean_close_checkpoints_once() -> Result<()> {
        let usage = Traffic::memory();
        let session = usage.session().await;
        let counter = usage.counter("network", Some("source"));
        for _ in 0..5000 {
            counter.record(64, false);
        }
        let db = usage.writer.db.as_ref().unwrap();
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM traffic_usage_daily", [], |r| r
                    .get::<_, i64>(0))?,
            0
        );
        usage.writer.flush()?;
        assert_eq!(
            usage
                .report("network", None, &TrafficQuery::default())?
                .totals
                .ingress_bytes,
            320_000
        );
        drop(session);
        usage.writer.flush()?;
        assert_eq!(
            db.lock().unwrap().query_row(
                "SELECT active_sessions FROM traffic_metadata",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            0
        );
        Ok(())
    }
    #[test]
    fn statistical_normal_sync_is_separate_from_job_durability_and_crashes_are_marked() -> Result<()>
    {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("traffic.sqlite");
        let usage = Traffic::open(&path);
        let db = usage.writer.db.as_ref().unwrap();
        assert_eq!(
            db.lock()
                .unwrap()
                .pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))?,
            1
        );
        let jobs = Connection::open_in_memory()?;
        crate::database::configure(&jobs)?;
        assert_eq!(
            jobs.pragma_query_value(None, "synchronous", |r| r.get::<_, i64>(0))?,
            2
        );
        db.lock()
            .unwrap()
            .execute("UPDATE traffic_metadata SET active_sessions=1", [])?;
        drop(usage);
        let recovered = Traffic::open(&path);
        assert!(
            !recovered
                .report("network", None, &TrafficQuery::default())?
                .complete
        );
        Ok(())
    }
    #[test]
    fn storage_failure_keeps_forwarding_enabled_and_marks_recovered_counters_incomplete()
    -> Result<()> {
        let usage = Traffic::memory();
        let db = usage.writer.db.as_ref().unwrap();
        db.lock().unwrap().execute_batch(
            "CREATE TRIGGER unavailable BEFORE INSERT ON traffic_usage_daily BEGIN SELECT RAISE(FAIL,'test storage error'); END;",
        )?;
        usage.record("n", Some("a"), 12, false);
        let report = usage.report("n", None, &TrafficQuery::default())?;
        assert!(!report.complete);
        assert_eq!(report.totals.ingress_bytes, 12);
        db.lock()
            .unwrap()
            .execute_batch("DROP TRIGGER unavailable")?;
        usage.record("n", Some("a"), 7, false);
        assert!(!usage.report("n", None, &TrafficQuery::default())?.complete);
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT incomplete FROM traffic_metadata", [], |r| r
                    .get::<_, i64>(0))?,
            1
        );
        Ok(())
    }
    #[test]
    fn usage_survives_reopen_and_calendar_boundaries_without_double_counting() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("traffic.sqlite");
        let old = 1_769_903_999_000; // 2026-01-31 23:59:59 UTC
        let now = old + 1000;
        let usage = Traffic::open(&path);
        usage.record_at("network", Some("a"), 100, false, old)?;
        usage.record_at("network", Some("b"), 100, true, old)?;
        usage.record_at("network", Some("b"), 30, false, now)?;
        usage.record_at("network", Some("a"), 30, true, now)?;
        usage.record_at("other", Some("a"), 999, false, now)?;
        drop(usage);
        let usage = Traffic::open(&path);
        let query = TrafficQuery {
            period: TrafficPeriod::Month,
            ..Default::default()
        };
        let report = usage.report_at("network", None, &query, now)?;
        assert_eq!(
            report.totals,
            TrafficBytes {
                ingress_bytes: 30,
                egress_bytes: 30
            }
        );
        assert_eq!(report.daily.len(), 1);
        assert_eq!(report.recorded_since_ms, Some(old));
        let all = usage.report_at(
            "network",
            None,
            &TrafficQuery {
                period: TrafficPeriod::All,
                limit: 1,
                offset: 0,
            },
            now,
        )?;
        assert_eq!(
            all.totals,
            TrafficBytes {
                ingress_bytes: 130,
                egress_bytes: 130
            }
        );
        assert_eq!(all.next_offset, Some(1));
        let own = usage.report_at(
            "network",
            Some("a"),
            &TrafficQuery {
                period: TrafficPeriod::All,
                ..query
            },
            now,
        )?;
        assert_eq!(
            own.totals,
            TrafficBytes {
                ingress_bytes: 100,
                egress_bytes: 30
            }
        );
        assert_eq!(own.devices.len(), 1);
        usage.record_at("network", None, 7, false, now)?;
        assert_eq!(
            usage
                .report_at("network", None, &TrafficQuery::default(), now)?
                .totals
                .ingress_bytes,
            37
        );
        Ok(())
    }
}
