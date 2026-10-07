import { expect, test } from "bun:test";
import { earlierTags, overlaps, protocolRange } from "./compatibility-baseline";

test("baseline uses SemVer ordering and excludes equal, future and malformed tags", () => {
  const tags = [
    "v0.0.1-beta.4",
    "v0.0.1",
    "v0.0.1-beta.10",
    "v0.0.2",
    "v0.0.1-beta.2",
    "notes",
    "v0.0.1-beta.01",
  ];
  expect(earlierTags(tags, "0.0.2")).toEqual([
    "v0.0.1",
    "v0.0.1-beta.10",
    "v0.0.1-beta.4",
    "v0.0.1-beta.2",
  ]);
  expect(earlierTags(tags, "0.0.1")).toEqual([
    "v0.0.1-beta.10",
    "v0.0.1-beta.4",
    "v0.0.1-beta.2",
  ]);
  expect(earlierTags(tags, "0.0.1-beta.5")).toEqual([
    "v0.0.1-beta.4",
    "v0.0.1-beta.2",
  ]);
  expect(earlierTags([], "0.0.1")).toEqual([]);
  expect(() => earlierTags(tags, "invalid")).toThrow();
});

test("unreadable protocol contracts fail instead of silently passing compatibility", () => {
  expect(protocolRange("pub const PROTOCOL: u32 = 2; min: 1")).toEqual({
    min: 1,
    max: 2,
  });
  expect(() => protocolRange("broken contract")).toThrow();
  expect(overlaps({ min: 1, max: 2 }, { min: 1, max: 1 })).toBe(true);
  expect(overlaps({ min: 1, max: 2 }, { min: 3, max: 3 })).toBe(false);
});
