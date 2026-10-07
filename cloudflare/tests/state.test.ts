import { expect, test } from "bun:test";
import { attachment, type SourceState } from "../src/state";
import { WINDOW } from "../src/limits";

test("restored roles require their own complete fields and reject malformed budgets", () => {
  const common = {
    id: crypto.randomUUID(),
    network: "probe",
    ip: "unknown",
    protocol: 2,
  };
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
    source: `dev_${"d".repeat(32)}`,
    management: false,
    cachedSince: null,
    peer: crypto.randomUUID(),
    outstanding: 0,
    deadline: Date.now() + 1000,
  };
  expect(attachment(live)).toEqual(live);
  const cachedSince = Date.now();
  expect(attachment({...live, cachedSince})).toEqual({...live, cachedSince});
  expect(attachment({...live, cachedSince, protocol: 1})).toBeUndefined();
  expect(attachment({...live, cachedSince, anonymous: true})).toBeUndefined();
  const {protocol: _protocol, cachedSince: _cached, ...legacy} = live;
  expect(attachment(legacy)).toEqual({...live, protocol: 1});
  const {source: _source, management: _management, ...oldAdmission} = live;
  expect(attachment(oldAdmission)).toEqual({...live, source: null});
  expect(attachment({...live, source: "invalid"})).toBeUndefined();
  expect(attachment({...live, source: null, management: true})).toBeUndefined();
  expect(attachment({...live, anonymous: true})).toBeUndefined();
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
