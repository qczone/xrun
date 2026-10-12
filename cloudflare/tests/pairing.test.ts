import { Database } from "bun:sqlite";
import { expect, test } from "bun:test";
import type { DurableObjectStorage } from "@cloudflare/workers-types";
import { Pairings } from "../src/pairing";

test("public peer throttles persist independently of locators and expire without resetting password budgets", () => {
  const database = new Database(":memory:");
  const storage = {
    sql: {
      exec(query: string, ...bindings: (string | number | null)[]) {
        const rows = database.query(query).all(...bindings);
        return { toArray: () => rows, one: () => rows[0] };
      },
    },
    transactionSync: (callback: () => unknown) =>
      database.transaction(callback)(),
  } as unknown as DurableObjectStorage;
  let pairings = new Pairings(storage);
  for (let attempt = 0; attempt < 32; attempt++)
    pairings.throttlePeer("peer", 1);
  expect(() => pairings.throttlePeer("peer", 2)).toThrow("rate exceeded");
  pairings = new Pairings(storage);
  expect(() => pairings.throttlePeer("peer", 3)).toThrow("rate exceeded");
  expect(() => pairings.throttlePeer("different-peer", 3)).not.toThrow();
  expect(() => pairings.throttlePeer("peer", 60_001)).not.toThrow();
  database.close();
});
