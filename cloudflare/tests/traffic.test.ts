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

test("UTC periods and anonymous/network totals remain correct after reopening SQLite", () => {
  const directory = mkdtempSync(join(tmpdir(), "xrun-traffic-"));
  try {
    const path = join(directory, "usage.sqlite");
    let database = new Database(path);
    let usage = new Traffic(storage(database));
    const old = Date.UTC(2026, 0, 31, 23, 59, 59);
    const now = old + 1000;
    usage.record("n", "a", 100, false, old);
    usage.record("n", "b", 100, true, old);
    usage.record("n", "b", 30, false, now);
    usage.record("n", "a", 30, true, now);
    usage.record("other", "a", 999, false, now);
    database.close();
    database = new Database(path);
    usage = new Traffic(storage(database));
    const month = { period: "month" as const, offset: 0, limit: 50 };
    const report = usage.report("n", null, month, now);
    expect(report.totals).toEqual({ ingress_bytes: 30, egress_bytes: 30 });
    expect(report.daily).toHaveLength(1);
    expect(report.recorded_since_ms).toBe(old);
    expect(
      usage.report("n", "a", { ...month, period: "all" }, now).totals,
    ).toEqual({ ingress_bytes: 100, egress_bytes: 30 });
    usage.record("n", null, 7, false, now);
    expect(usage.report("n", null, month, now).totals.ingress_bytes).toBe(37);
    database.close();
    expect(trafficQuery({ period: null })).toBeUndefined();
    expect(trafficQuery({ limit: null })).toBeUndefined();
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("recording errors do not escape and incomplete counters survive recovery and reopen", () => {
  const directory = mkdtempSync(join(tmpdir(), "xrun-traffic-"));
  const path = join(directory, "usage.sqlite");
  let database = new Database(path);
  try {
    let usage = new Traffic(storage(database));
    database.exec(
      "CREATE TRIGGER unavailable BEFORE INSERT ON traffic_usage_daily BEGIN SELECT RAISE(FAIL,'test storage error'); END;",
    );
    const query = { period: "all" as const, offset: 0, limit: 50 };
    expect(() => usage.record("n", "a", 12, false)).not.toThrow();
    const failed = usage.report("n", null, query);
    expect(failed.complete).toBe(false);
    expect(failed.totals.ingress_bytes).toBe(0);
    database.exec("DROP TRIGGER unavailable");
    usage.record("n", "a", 7, false);
    database.close();
    database = new Database(path);
    usage = new Traffic(storage(database));
    const recovered = usage.report("n", null, query);
    expect(recovered.complete).toBe(false);
    expect(recovered.totals.ingress_bytes).toBe(7);
  } finally {
    database.close();
    rmSync(directory, { recursive: true, force: true });
  }
});
