//! Durable daily counters. Recording failures never interrupt ciphertext forwarding.
use super::*;
use crate::protocol::{
    DeviceTraffic, TrafficBytes, TrafficDay, TrafficPeriod, TrafficQuery, TrafficReport,
};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicBool, Ordering},
};

const DAY_MS: i64 = 86_400_000;
const SCHEMA: &str = "
CREATE TABLE traffic_usage_daily (
    network_id TEXT NOT NULL,
    device_id TEXT NOT NULL,
    day_ms INTEGER NOT NULL,
    ingress_bytes INTEGER NOT NULL CHECK(ingress_bytes >= 0),
    egress_bytes INTEGER NOT NULL CHECK(egress_bytes >= 0),
    first_seen_ms INTEGER NOT NULL,
    PRIMARY KEY(network_id, device_id, day_ms)
) STRICT;
CREATE TABLE traffic_metadata (id INTEGER PRIMARY KEY CHECK(id=1), incomplete INTEGER NOT NULL);
INSERT INTO traffic_metadata VALUES(1, 0);
";
const ADD: &str = "INSERT INTO traffic_usage_daily VALUES(?1,?2,?3,?4,?5,?6)
ON CONFLICT(network_id,device_id,day_ms) DO UPDATE SET
ingress_bytes=ingress_bytes+excluded.ingress_bytes,
egress_bytes=egress_bytes+excluded.egress_bytes";

pub(super) struct Traffic {
    db: std::result::Result<StdMutex<Connection>, String>,
    incomplete: AtomicBool,
    pub(super) persistent: bool,
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
            crate::database::initialize(&mut db, "relay traffic", 1, SCHEMA)?;
            Ok(db)
        })();
        Self::from_database(result, true)
    }
    fn from_database(result: Result<Connection>, persistent: bool) -> Self {
        if let Err(error) = &result {
            tracing::warn!(%error, "relay traffic storage unavailable; forwarding remains enabled");
        }
        Self {
            db: result.map(StdMutex::new).map_err(|e| e.to_string()),
            incomplete: AtomicBool::new(false),
            persistent,
        }
    }
    #[cfg(test)]
    pub(super) fn memory() -> Self {
        Self::from_database(
            (|| {
                let mut db = Connection::open_in_memory()?;
                crate::database::initialize(&mut db, "relay traffic", 1, SCHEMA)?;
                Ok(db)
            })(),
            false,
        )
    }
    pub(super) fn record(&self, network: &str, device: Option<&str>, bytes: usize, outgoing: bool) {
        if bytes == 0 || self.db.is_err() {
            return;
        }
        if let Err(error) = self.record_at(network, device, bytes, outgoing, now_ms())
            && !self.incomplete.swap(true, Ordering::Relaxed)
        {
            if let Ok(db) = &self.db
                && let Ok(db) = db.lock()
            {
                let _ = db.execute("UPDATE traffic_metadata SET incomplete=1 WHERE id=1", []);
            }
            tracing::warn!(%error, "relay traffic recording failed; forwarding remains enabled");
        }
    }
    pub(super) fn recording_task_failed(&self) {
        self.incomplete.store(true, Ordering::Relaxed);
    }
    fn record_at(
        &self,
        network: &str,
        device: Option<&str>,
        bytes: usize,
        outgoing: bool,
        now: i64,
    ) -> Result<()> {
        let db = self
            .db
            .as_ref()
            .map_err(|error| anyhow::anyhow!(ErrorCode::StorageError.error(error)))?;
        let mut db = db.lock().map_err(|_| {
            anyhow::anyhow!(ErrorCode::StorageError.error("traffic storage lock failed"))
        })?;
        let transaction = db.transaction()?;
        if self.incomplete.load(Ordering::Relaxed) {
            transaction.execute("UPDATE traffic_metadata SET incomplete=1 WHERE id=1", [])?;
        }
        let day = now.div_euclid(DAY_MS) * DAY_MS;
        for device in std::iter::once("").chain(device.filter(|id| !id.is_empty())) {
            let (ingress, egress) = if outgoing {
                (0, bytes as i64)
            } else {
                (bytes as i64, 0)
            };
            transaction
                .prepare_cached(ADD)?
                .execute(params![network, device, day, ingress, egress, now])?;
        }
        transaction.commit()?;
        Ok(())
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
        let db = self
            .db
            .as_ref()
            .map_err(|error| anyhow::anyhow!(ErrorCode::StorageError.error(error)))?;
        let mut db = db.lock().map_err(|_| {
            anyhow::anyhow!(ErrorCode::StorageError.error("traffic storage lock failed"))
        })?;
        let transaction = db.transaction()?;
        let start = start_ms(query.period, now)?;
        let key = device.unwrap_or("");
        let totals = transaction.query_row(
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
        let recorded_since_ms = transaction.query_row(
            "SELECT MIN(first_seen_ms) FROM traffic_usage_daily WHERE network_id=?1 AND device_id=?2",
            params![network, key], |row| row.get(0),
        )?;
        let complete = !self.incomplete.load(Ordering::Relaxed)
            && transaction.query_row(
                "SELECT incomplete=0 FROM traffic_metadata WHERE id=1",
                [],
                |row| row.get::<_, bool>(0),
            )?;
        let mut statement = transaction.prepare(
            "SELECT device_id,SUM(ingress_bytes),SUM(egress_bytes) FROM traffic_usage_daily
             WHERE network_id=?1 AND device_id<>'' AND (?2 IS NULL OR device_id=?2) AND day_ms>=?3
             GROUP BY device_id ORDER BY device_id LIMIT ?4 OFFSET ?5",
        )?;
        let mut devices = statement
            .query_map(
                params![network, device, start, query.limit as u32 + 1, query.offset],
                |row| {
                    Ok(DeviceTraffic {
                        device_id: row.get(0)?,
                        sent_bytes: row.get::<_, i64>(1)? as u64,
                        received_bytes: row.get::<_, i64>(2)? as u64,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let next_offset = if devices.len() > query.limit as usize {
            devices.pop();
            query.offset.checked_add(query.limit as u32)
        } else {
            None
        };
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
    Ok(negotiated(
        ws.max_message_size(MAX_MESSAGE)
            .max_frame_size(MAX_MESSAGE)
            .on_upgrade(move |mut ws| async move {
                let result = async {
                    let path = format!("/networks/{network}/traffic");
                    let (device, manager) = member(&mut ws, &network, &path, &permit).await?;
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
    #[test]
    fn storage_failure_keeps_forwarding_enabled_and_marks_recovered_counters_incomplete()
    -> Result<()> {
        let usage = Traffic::memory();
        let db = usage.db.as_ref().unwrap();
        db.lock().unwrap().execute_batch(
            "CREATE TRIGGER unavailable BEFORE INSERT ON traffic_usage_daily BEGIN SELECT RAISE(FAIL,'test storage error'); END;",
        )?;
        usage.record("n", Some("a"), 12, false);
        let report = usage.report("n", None, &TrafficQuery::default())?;
        assert!(!report.complete);
        assert_eq!(report.totals.ingress_bytes, 0);
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
