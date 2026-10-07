import { expect, test } from "bun:test";
import {
  negotiate,
  parseRange,
  PROTOCOL,
  SIGNATURE_FORMAT,
} from "../src/protocol";
import vectors from "../../tests/fixtures/signatures.json";

test("wire versions select the highest common protocol and reject malformed ranges", () => {
  expect(negotiate(PROTOCOL, vectors.protocol)).toBe(1);
  expect(SIGNATURE_FORMAT).toBe(vectors.signature_format);
  expect(negotiate({ min: 2, max: 3 }, { min: 1, max: 2 })).toBe(2);
  for (const invalid of [
    "",
    "1",
    "-1-2",
    "0-1",
    "2-1",
    "1-2-3",
    "1-+2",
    "1-4294967296",
  ])
    expect(() => parseRange(invalid)).toThrow();
  for (const range of [
    { min: 0, max: 1 },
    { min: 3, max: 2 },
    { min: PROTOCOL.max + 1, max: PROTOCOL.max + 1 },
  ])
    expect(() => negotiate(PROTOCOL, range)).toThrow();
});
