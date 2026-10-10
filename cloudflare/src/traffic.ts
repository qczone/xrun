/** Persisted ciphertext counters. Control messages and frame contents are never recorded. */
import type { DurableObjectStorage } from "@cloudflare/workers-types";
export type TrafficPeriod = "today" | "month" | "all";
export interface TrafficQuery {
  period: TrafficPeriod;
  offset: number;
  limit: number;
}
type Bytes = {
  ingress_bytes: number;
  egress_bytes: number;
};
interface Daily extends Bytes {
  start_ms: number;
}
const DAY = 86_400_000;
const ADD = `INSERT INTO traffic_usage_daily VALUES(?,?,?,?,?,?)
ON CONFLICT(network_id,device_id,day_ms) DO UPDATE SET
ingress_bytes=ingress_bytes+excluded.ingress_bytes,
egress_bytes=egress_bytes+excluded.egress_bytes`;

export function trafficQuery(value: unknown): TrafficQuery | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return;
  const query = value as Record<string, unknown>;
  const period = query.period === undefined ? "month" : query.period;
  const offset = query.offset === undefined ? 0 : query.offset;
  const limit = query.limit === undefined ? 50 : query.limit;
  if (
    (period !== "today" && period !== "month" && period !== "all") ||
    typeof offset !== "number" ||
    !Number.isInteger(offset) ||
    offset < 0 ||
    offset > 0xffffffff ||
    typeof limit !== "number" ||
    !Number.isInteger(limit) ||
    limit < 1 ||
    limit > 256
  )
    return;
  return { period, offset, limit };
}

export class Traffic {
  private incomplete = false;
  private unavailable: unknown = null;
  constructor(private readonly storage: DurableObjectStorage) {
    try {
      storage.sql.exec(`CREATE TABLE IF NOT EXISTS traffic_usage_daily (
        network_id TEXT NOT NULL, device_id TEXT NOT NULL, day_ms INTEGER NOT NULL,
        ingress_bytes INTEGER NOT NULL CHECK(ingress_bytes>=0),
        egress_bytes INTEGER NOT NULL CHECK(egress_bytes>=0), first_seen_ms INTEGER NOT NULL,
        PRIMARY KEY(network_id,device_id,day_ms)
      ) STRICT;
      CREATE TABLE IF NOT EXISTS traffic_metadata (id INTEGER PRIMARY KEY CHECK(id=1), incomplete INTEGER NOT NULL);
      INSERT OR IGNORE INTO traffic_metadata VALUES(1,0)`);
    } catch (error) {
      this.unavailable = error;
      console.warn(
        "Relay traffic storage unavailable; forwarding remains enabled",
        error,
      );
    }
  }
  record(
    network: string,
    device: string | null,
    bytes: number,
    outgoing: boolean,
    now = Date.now(),
  ): void {
    if (!bytes || this.unavailable) return;
    try {
      this.storage.transactionSync(() => {
        if (this.incomplete)
          this.storage.sql.exec(
            "UPDATE traffic_metadata SET incomplete=1 WHERE id=1",
          );
        const day = Math.floor(now / DAY) * DAY;
        for (const key of device ? ["", device] : [""]) {
          this.storage.sql.exec(
            ADD,
            network,
            key,
            day,
            outgoing ? 0 : bytes,
            outgoing ? bytes : 0,
            now,
          );
        }
      });
    } catch (error) {
      if (!this.incomplete)
        console.warn(
          "Relay traffic recording failed; forwarding remains enabled",
          error,
        );
      this.incomplete = true;
      try {
        this.storage.sql.exec(
          "UPDATE traffic_metadata SET incomplete=1 WHERE id=1",
        );
      } catch {
        // A full or unavailable store may also prevent saving the failure marker.
      }
    }
  }
  report(
    network: string,
    device: string | null,
    query: TrafficQuery,
    now = Date.now(),
  ) {
    if (this.unavailable) throw this.unavailable;
    const day = Math.floor(now / DAY) * DAY;
    const date = new Date(now);
    const start =
      query.period === "all"
        ? 0
        : query.period === "today"
          ? day
          : Date.UTC(date.getUTCFullYear(), date.getUTCMonth(), 1);
    const key = device ?? "";
    const totals = this.storage.sql
      .exec<Bytes>(
        `SELECT COALESCE(SUM(ingress_bytes),0) AS ingress_bytes,COALESCE(SUM(egress_bytes),0) AS egress_bytes
       FROM traffic_usage_daily WHERE network_id=? AND device_id=? AND day_ms>=?`,
        network,
        key,
        start,
      )
      .one();
    const since = this.storage.sql
      .exec<{ since: number | null }>(
        "SELECT MIN(first_seen_ms) AS since FROM traffic_usage_daily WHERE network_id=? AND device_id=?",
        network,
        key,
      )
      .one();
    const complete =
      !this.incomplete &&
      this.storage.sql
        .exec<{ incomplete: number }>(
          "SELECT incomplete FROM traffic_metadata WHERE id=1",
        )
        .one().incomplete === 0;
    const devices = this.storage.sql
      .exec<{ device_id: string; sent_bytes: number; received_bytes: number }>(
        `SELECT device_id,SUM(ingress_bytes) AS sent_bytes,SUM(egress_bytes) AS received_bytes
       FROM traffic_usage_daily WHERE network_id=? AND device_id<>'' AND (? IS NULL OR device_id=?) AND day_ms>=?
       GROUP BY device_id ORDER BY device_id LIMIT ? OFFSET ?`,
        network,
        device,
        device,
        start,
        query.limit + 1,
        query.offset,
      )
      .toArray();
    const next =
      devices.length > query.limit ? query.offset + query.limit : null;
    if (next !== null) devices.pop();
    const historyStart = query.period === "all" ? day - 29 * DAY : start;
    const rows = this.storage.sql
      .exec<{ day_ms: number } & Bytes>(
        "SELECT day_ms,ingress_bytes,egress_bytes FROM traffic_usage_daily WHERE network_id=? AND device_id=? AND day_ms>=?",
        network,
        key,
        historyStart,
      )
      .toArray();
    const byDay = new Map(rows.map((row) => [row.day_ms, row]));
    const daily: Daily[] = [];
    for (let current = historyStart; current <= day; current += DAY) {
      const row = byDay.get(current);
      daily.push({
        start_ms: current,
        ingress_bytes: row?.ingress_bytes ?? 0,
        egress_bytes: row?.egress_bytes ?? 0,
      });
    }
    return {
      network_id: network,
      device_id: device,
      period: query.period,
      start_ms: start,
      end_ms: now,
      recorded_since_ms: since.since,
      complete,
      totals,
      devices,
      daily,
      next_offset: next,
    };
  }
}
