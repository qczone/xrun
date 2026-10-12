/** Persisted ciphertext counters. Control messages and frame contents are never recorded. */
import type { DurableObjectStorage } from "@cloudflare/workers-types";
import {
  trafficCounters,
  TRAFFIC_DAY,
  type TrafficCounter,
  type TrafficLeg,
} from "./traffic-state";
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
const DAY = TRAFFIC_DAY;
const deviceOf = (leg: TrafficLeg) =>
  (leg.role === "source" ? leg.source : leg.target) ?? "";
const ADD = `INSERT INTO traffic_usage_daily VALUES(?,?,?,?,?,?)
ON CONFLICT(network_id,device_id,day_ms) DO UPDATE SET
ingress_bytes=ingress_bytes+excluded.ingress_bytes,
egress_bytes=egress_bytes+excluded.egress_bytes,
first_seen_ms=MIN(first_seen_ms,excluded.first_seen_ms)`;

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
  nextFlush: number | null = null;
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
      INSERT OR IGNORE INTO traffic_metadata VALUES(1,0);
      CREATE TABLE IF NOT EXISTS traffic_checkpoints (
        id TEXT PRIMARY KEY, network_id TEXT NOT NULL, device_id TEXT NOT NULL, data TEXT NOT NULL
      ) STRICT`);
    } catch (error) {
      this.unavailable = error;
      console.warn(
        "Relay traffic storage unavailable; forwarding remains enabled",
        error,
      );
    }
  }
  /** A durable active marker is written once at tunnel creation, never per frame. */
  begin(leg: TrafficLeg): void {
    if (this.unavailable) return;
    try {
      this.storage.sql.exec(
        "INSERT INTO traffic_checkpoints VALUES(?,?,?,?)",
        leg.id,
        leg.network,
        deviceOf(leg),
        JSON.stringify(leg.traffic),
      );
    } catch (error) {
      this.fail(error);
    }
  }
  private fail(error: unknown): void {
    if (!this.incomplete)
      console.warn(
        "Relay counters incomplete; forwarding remains enabled",
        error,
      );
    this.incomplete = true;
  }
  recordLeg(
    leg: TrafficLeg,
    bytes: number,
    outgoing: boolean,
    now = Date.now(),
  ): TrafficCounter[] {
    const cells = leg.traffic.map((counter) => ({ ...counter }));
    const day = Math.floor(now / DAY) * DAY;
    let cell = cells.find((counter) => counter.day_ms === day);
    if (!cell) {
      if (cells.length === 2) {
        if (!this.checkpoint([leg]))
          this.fail("UTC rollover while counters unavailable");
        cells.shift();
      }
      cell = {
        day_ms: day,
        ingress_bytes: 0,
        egress_bytes: 0,
        first_seen_ms: now,
      };
      cells.push(cell);
      cells.sort((a, b) => a.day_ms - b.day_ms);
    }
    const field = outgoing ? "egress_bytes" : "ingress_bytes";
    if (
      !Number.isSafeInteger(bytes) ||
      bytes <= 0 ||
      !Number.isSafeInteger(cell[field] + bytes)
    ) {
      this.fail("traffic counter overflow");
      return leg.traffic;
    }
    cell[field] += bytes;
    this.nextFlush ??= now + 10_000;
    return cells;
  }
  private previous(leg: TrafficLeg): TrafficCounter[] {
    const row = this.storage.sql
      .exec<{ network_id: string; device_id: string; data: string }>(
        "SELECT network_id,device_id,data FROM traffic_checkpoints WHERE id=?",
        leg.id,
      )
      .toArray()[0];
    if (!row) return [];
    if (row.network_id !== leg.network || row.device_id !== deviceOf(leg))
      throw new Error("Traffic binding changed");
    const counters = trafficCounters(JSON.parse(row.data));
    if (!counters) throw new Error("Invalid persisted traffic checkpoint");
    return counters;
  }
  private delta(leg: TrafficLeg): TrafficCounter[] {
    const previous = this.previous(leg);
    return leg.traffic.map((cell) => {
      const old = previous.find((old) => old.day_ms === cell.day_ms);
      const ingress = cell.ingress_bytes - (old?.ingress_bytes ?? 0);
      const egress = cell.egress_bytes - (old?.egress_bytes ?? 0);
      if (ingress < 0 || egress < 0)
        throw new Error("Traffic counter rollback");
      return { ...cell, ingress_bytes: ingress, egress_bytes: egress };
    });
  }
  /** Cumulative snapshots are idempotent across alarm retries and isolate eviction. */
  checkpoint(legs: TrafficLeg[], finished: string[] = []): boolean {
    if (this.unavailable) return false;
    try {
      this.storage.transactionSync(() => {
        if (this.incomplete)
          this.storage.sql.exec(
            "UPDATE traffic_metadata SET incomplete=1 WHERE id=1",
          );
        for (const leg of legs) {
          const deltas = this.delta(leg).filter(
            (cell) => cell.ingress_bytes || cell.egress_bytes,
          );
          for (const cell of deltas) {
            for (const key of deviceOf(leg) ? ["", deviceOf(leg)] : [""]) {
              this.storage.sql.exec(
                ADD,
                leg.network,
                key,
                cell.day_ms,
                cell.ingress_bytes,
                cell.egress_bytes,
                cell.first_seen_ms,
              );
            }
          }
          if (finished.includes(leg.id))
            this.storage.sql.exec(
              "DELETE FROM traffic_checkpoints WHERE id=?",
              leg.id,
            );
          else if (deltas.length)
            this.storage.sql.exec(
              "INSERT INTO traffic_checkpoints VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
              leg.id,
              leg.network,
              deviceOf(leg),
              JSON.stringify(leg.traffic),
            );
        }
      });
      return true;
    } catch (error) {
      this.fail(error);
      try {
        this.storage.sql.exec(
          "UPDATE traffic_metadata SET incomplete=1 WHERE id=1",
        );
      } catch {
        /* Retry at the next checkpoint. */
      }
      return false;
    }
  }
  flush(legs: TrafficLeg[]): void {
    this.checkpoint(legs);
    this.nextFlush = null;
  }
  finish(leg: TrafficLeg): void {
    this.checkpoint([leg], [leg.id]);
  }
  recover(legs: TrafficLeg[]): void {
    if (this.unavailable) return;
    try {
      const live = new Set(legs.map((leg) => leg.id));
      const missing = this.storage.sql
        .exec<{ id: string }>("SELECT id FROM traffic_checkpoints")
        .toArray()
        .filter((row) => !live.has(row.id));
      if (missing.length) {
        this.fail(
          "active socket attachments were lost; the last checkpoint may omit traffic",
        );
        this.storage.transactionSync(() => {
          for (const row of missing)
            this.storage.sql.exec(
              "DELETE FROM traffic_checkpoints WHERE id=?",
              row.id,
            );
          this.storage.sql.exec(
            "UPDATE traffic_metadata SET incomplete=1 WHERE id=1",
          );
        });
      }
      this.flush(legs);
    } catch (error) {
      this.fail(error);
    }
  }
  report(
    network: string,
    device: string | null,
    query: TrafficQuery,
    now = Date.now(),
    legs: TrafficLeg[] = [],
  ) {
    if (this.unavailable) throw this.unavailable;
    this.flush(legs);
    const pending = legs
      .filter((leg) => leg.network === network)
      .flatMap((leg) =>
        this.delta(leg).map((cell) => ({ ...cell, device: deviceOf(leg) })),
      );
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
       GROUP BY device_id ORDER BY device_id`,
        network,
        device,
        device,
        start,
      )
      .toArray();
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
    for (const cell of pending) {
      if (device === null || cell.device === device) {
        if (cell.ingress_bytes || cell.egress_bytes)
          since.since =
            since.since === null
              ? cell.first_seen_ms
              : Math.min(since.since, cell.first_seen_ms);
        if (cell.day_ms >= start) {
          totals.ingress_bytes += cell.ingress_bytes;
          totals.egress_bytes += cell.egress_bytes;
        }
        if (cell.day_ms >= historyStart) {
          const row = byDay.get(cell.day_ms) ?? {
            day_ms: cell.day_ms,
            ingress_bytes: 0,
            egress_bytes: 0,
          };
          row.ingress_bytes += cell.ingress_bytes;
          row.egress_bytes += cell.egress_bytes;
          byDay.set(cell.day_ms, row);
        }
      }
      if (
        cell.device &&
        cell.day_ms >= start &&
        (device === null || device === cell.device)
      ) {
        let row = devices.find((row) => row.device_id === cell.device);
        if (!row) {
          row = { device_id: cell.device, sent_bytes: 0, received_bytes: 0 };
          devices.push(row);
        }
        row.sent_bytes += cell.ingress_bytes;
        row.received_bytes += cell.egress_bytes;
      }
    }
    devices.sort((a, b) => a.device_id.localeCompare(b.device_id));
    devices.splice(0, query.offset);
    const next =
      devices.length > query.limit ? query.offset + query.limit : null;
    devices.splice(query.limit);
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
