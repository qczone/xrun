import { Database } from "bun:sqlite";
import type { DurableObjectStorage } from "@cloudflare/workers-types";
import { expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Traffic, trafficQuery } from "../src/traffic";

// The real workerd route is covered by relay.test.ts; this clock-controlled
// SQLite adapter exercises UTC boundaries and the persisted counter layout.
function storage(database: Database): DurableObjectStorage {
  return {
    sql: {
      exec(sql: string, ...bindings: (string | number | null)[]) {
        if (sql.startsWith("CREATE TABLE")) database.exec(sql);
        const rows = sql.startsWith("CREATE TABLE")
          ? []
          : database.query(sql).all(...bindings);
        return {
          toArray: () => rows,
          one: () => {
            if (rows.length !== 1) throw new Error("expected one row");
            return rows[0];
          },
        };
      },
    },
    transactionSync: (callback: () => unknown) =>
      database.transaction(callback)(),
  } as unknown as DurableObjectStorage;
}

function leg(
  id: string,
  network = "n",
  device: string | null = "a",
): import("../src/traffic-state").TrafficLeg {
  return {
    id,
    network,
    role: "source",
    source: device,
    target: "target",
    traffic: [],
  };
}
function record(
  usage: Traffic,
  state: ReturnType<typeof leg>,
  bytes: number,
  outgoing: boolean,
  now?: number,
) {
  state.traffic = usage.recordLeg(state, bytes, outgoing, now);
}
test("UTC checkpoints survive hibernation, retries and reopening without counting twice", () => {
  const directory = mkdtempSync(join(tmpdir(), "xrun-traffic-"));
  const path = join(directory, "usage.sqlite");
  let database = new Database(path);
  try {
    let usage = new Traffic(storage(database));
    const a = leg("a"),
      b = leg("b", "n", "b"),
      other = leg("other", "other"),
      anonymous = leg("anonymous", "n", null);
    const legs = [a, b, other, anonymous];
    legs.forEach((state) => usage.begin(state));
    const old = Date.UTC(2026, 0, 31, 23, 59, 59),
      now = old + 1000;
    record(usage, a, 100, false, old);
    record(usage, b, 100, true, old);
    record(usage, b, 30, false, now);
    record(usage, a, 30, true, now);
    record(usage, other, 999, false, now);
    expect(
      database.query("SELECT COUNT(*) as count FROM traffic_usage_daily").get(),
    ).toEqual({ count: 0 });
    usage.flush(legs);
    usage.flush(legs);
    database.close();
    database = new Database(path);
    usage = new Traffic(storage(database));
    usage.recover(legs);
    const month = { period: "month" as const, offset: 0, limit: 50 };
    const report = usage.report("n", null, month, now, legs);
    expect(report.totals).toEqual({ ingress_bytes: 30, egress_bytes: 30 });
    expect(report.daily).toHaveLength(1);
    expect(report.recorded_since_ms).toBe(old);
    expect(report.complete).toBe(true);
    expect(
      usage.report("n", "a", { ...month, period: "all" }, now, legs).totals,
    ).toEqual({ ingress_bytes: 100, egress_bytes: 30 });
    record(usage, anonymous, 7, false, now);
    expect(usage.report("n", null, month, now, legs).totals.ingress_bytes).toBe(
      37,
    );
    legs.forEach((state) => usage.finish(state));
    expect(
      database.query("SELECT COUNT(*) as count FROM traffic_checkpoints").get(),
    ).toEqual({ count: 0 });
    expect(trafficQuery({ period: null })).toBeUndefined();
    expect(trafficQuery({ limit: null })).toBeUndefined();
  } finally {
    database.close();
    rmSync(directory, { recursive: true, force: true });
  }
});
test("failed batch writes keep live totals and mark recovered counters incomplete", () => {
  const database = new Database(":memory:");
  try {
    const usage = new Traffic(storage(database)),
      state = leg("a");
    usage.begin(state);
    database.exec(
      "CREATE TRIGGER unavailable BEFORE INSERT ON traffic_usage_daily BEGIN SELECT RAISE(FAIL,'test storage error'); END;",
    );
    const query = { period: "all" as const, offset: 0, limit: 50 };
    record(usage, state, 12, false);
    const failed = usage.report("n", null, query, Date.now(), [state]);
    expect(failed.complete).toBe(false);
    expect(failed.totals.ingress_bytes).toBe(12);
    database.exec("DROP TRIGGER unavailable");
    record(usage, state, 7, false);
    usage.flush([state]);
    expect(usage.report("n", null, query).totals.ingress_bytes).toBe(19);
    const recovered = new Traffic(storage(database));
    recovered.recover([state]);
    expect(recovered.report("n", null, query).complete).toBe(false);
  } finally {
    database.close();
  }
});
test("lost active attachments mark the last checkpoint incomplete, clean closure does not", () => {
  const database = new Database(":memory:");
  try {
    let usage = new Traffic(storage(database)),
      state = leg("a");
    usage.begin(state);
    record(usage, state, 12, false);
    usage = new Traffic(storage(database));
    usage.recover([]);
    expect(
      usage.report("n", null, { period: "all", offset: 0, limit: 50 }).complete,
    ).toBe(false);
    expect(
      database.query("SELECT COUNT(*) as count FROM traffic_checkpoints").get(),
    ).toEqual({ count: 0 });
  } finally {
    database.close();
  }
});
