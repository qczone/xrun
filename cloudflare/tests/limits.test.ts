import { expect, test } from "bun:test";
import { FRAME, WINDOW } from "../src/limits";
import contract from "../../tests/fixtures/relay-limits.json";

test("relay framing matches the shared Rust contract", () => {
  expect({ frameBytes: FRAME, windowBytes: WINDOW }).toEqual(contract);
});
