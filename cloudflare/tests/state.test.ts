import { expect, test } from "bun:test";
import { attachment, type SourceState } from "../src/state";
import { WINDOW } from "../src/limits";

test("restored roles require their own complete fields and reject malformed budgets", () => {
  const common = { id: crypto.randomUUID(), network: "probe", ip: "unknown" };
  const binding = {
    target: `dev_${"a".repeat(32)}`,
    generation: "b".repeat(32),
    sid: "c".repeat(32),
  };
  const live: SourceState = {
    ...common,
    ...binding,
    role: "source",
    anonymous: false,
    peer: crypto.randomUUID(),
    outstanding: 0,
    deadline: Date.now() + 1000,
  };
  expect(attachment(live)).toEqual(live);
  for (const outstanding of [-1, WINDOW + 1, 1.5, NaN, Infinity, "0"]) {
    expect(attachment({ ...live, outstanding })).toBeUndefined();
  }
  for (const missing of [
    "sid",
    "generation",
    "peer",
    "anonymous",
    "deadline",
  ]) {
    const fields: Record<string, unknown> = { ...live };
    delete fields[missing];
    expect(attachment(fields)).toBeUndefined();
  }
  expect(attachment({ ...common, role: "closed" })).toEqual({
    ...common,
    role: "closed",
  });
  expect(attachment({ ...live, role: "closed" })).toBeUndefined();
  expect(
    attachment({ ...common, role: "control", device: binding.target }),
  ).toBeUndefined();
  expect(
    attachment({
      ...common,
      role: "auth",
      action: "connect",
      path: "/networks/probe/status",
      nonce: "a".repeat(26),
      deadline: Date.now(),
    }),
  ).toBeUndefined();
});
