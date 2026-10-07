import { expect, test } from "bun:test";
import { FRAME, WINDOW, SESSIONS, SESSIONS_PER_SOURCE, MANAGER_RESERVED_SESSIONS } from "../src/limits";
import contract from "../../tests/fixtures/relay-limits.json";

test("relay framing matches the shared Rust contract", () => {
  expect({ frameBytes: FRAME, windowBytes: WINDOW }).toEqual(contract);
});

test("CF source fairness leaves useful ordinary and manager capacity inside the memory budget", () => {
  expect(SESSIONS_PER_SOURCE).toBeGreaterThan(0);
  expect(SESSIONS_PER_SOURCE).toBeLessThan(SESSIONS - MANAGER_RESERVED_SESSIONS);
  expect(MANAGER_RESERVED_SESSIONS).toBeGreaterThan(0);
  expect(MANAGER_RESERVED_SESSIONS).toBeLessThan(SESSIONS);
  expect(2 * SESSIONS * WINDOW).toBe(64 * 1024 * 1024);
});
