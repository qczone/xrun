import { afterEach, beforeEach, expect, test } from "bun:test";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { Socket } from "./socket";
import { fixture, VERSION } from "./fixtures";
import { PROTOCOL, PROTOCOL_HEADER } from "../src/protocol";
import { RELAY_OBJECT } from "../src/routes";
import { SESSIONS_PER_SOURCE } from "../src/limits";
import { region, edgeRegion } from "../src/regions";
const secret = "a".repeat(26);
let runtime: Miniflare;
let origin: string;
beforeEach(async () => {
  const bundle = await Bun.build({
    entrypoints: [`${import.meta.dir}/workerd/shard-worker.ts`],
    target: "browser",
    external: ["cloudflare:workers"],
  });
  if (!bundle.success)
    throw new AggregateError(bundle.logs, "Shard fixture build failed");
  runtime = new Miniflare(
    convertV4MiniflareOptions({
      modules: true,
      script: await bundle.outputs[0].text(),
      compatibilityDate: "2026-10-03",
      durableObjects: {
        NETWORKS: { className: "TestShardRelay", useSQLite: true },
        DATA: { className: "TestShardData", useSQLite: true },
      },
      bindings: {
        RELAY_ROUTE: secret,
        XRUN_PROTOCOL_MIN: PROTOCOL.min,
        XRUN_PROTOCOL_MAX: PROTOCOL.max,
        XRUN_RELAY_REGION: "apac",
      },
    }),
  );
  origin = (await runtime.ready).origin;
});
afterEach(async () => {
  await runtime?.dispose();
});
async function response(path: string) {
  return runtime.dispatchFetch(
    `${origin}${path.startsWith("/.well-known/") ? "" : `/${secret}`}${path}`,
    {
      headers: {
        Upgrade: "websocket",
        "X-Xrun-Version": VERSION,
        "X-Xrun-Protocol": PROTOCOL_HEADER,
      },
    },
  );
}
async function open(path: string) {
  const reply = await response(path);
  if (!reply.webSocket)
    throw new Error(`Upgrade ${reply.status}: ${await reply.text()}`);
  expect(reply.headers.get("X-Xrun-Protocol")).toBe(String(PROTOCOL.max));
  return new Socket(reply.webSocket);
}
async function auth(path: string, proof: (nonce: string) => Promise<unknown>) {
  const socket = await open(path);
  const challenge = await socket.json();
  socket.send({ type: "authenticate", proof: await proof(challenge.nonce) });
  return socket;
}
async function inspect(
  binding: "NETWORKS" | "DATA",
  name: string,
  path = "metrics",
) {
  const namespace = await runtime.getDurableObjectNamespace(binding);
  return (
    await namespace
      .get(namespace.idFromName(name))
      .fetch(`https://test/test/${path}`)
  ).json() as Promise<any>;
}
async function network() {
  const f = await fixture();
  const manager = await f.member();
  const source = await f.member();
  const target = await f.member();
  const path = `/networks/${f.network}/claim`;
  const claim = await auth(path, (nonce) => manager.proof(path, nonce, true));
  expect((await claim.json()).type).toBe("relay_info");
  claim.close();
  async function control(member: typeof manager) {
    const path = `/networks/${f.network}/control`;
    const socket = await auth(path, (nonce) =>
      member.proof(path, nonce, member === manager),
    );
    const hello = await socket.json();
    return { socket, generation: hello.generation };
  }
  const controlled = await control(target);
  return { ...f, manager, source, target, controlled, control };
}
async function cold(f: Awaited<ReturnType<typeof network>>, member = f.source) {
  const path = `/networks/${f.network}/connect/${f.target.device}`;
  const coordinator = await auth(path, (nonce) =>
    member.proof(path, nonce, member === f.manager),
  );
  const route = await coordinator.json();
  if (route.type === "error") return { error: route.code } as const;
  expect(route.type).toBe("data_route");
  const incoming = await f.controlled.socket.json();
  expect(incoming.type).toBe("incoming");
  const attachment = await open(
    `/networks/${f.network}/attach/${f.target.device}/${f.controlled.generation}/${incoming.session_id}`,
  );
  const targetRoute = await attachment.json();
  const target = await open(targetRoute.path);
  const source = await open(route.path);
  expect((await target.json()).type).toBe("connected");
  expect((await source.json()).type).toBe("connected");
  return {
    source,
    target,
    route: route.path as string,
    sid: incoming.session_id as string,
  } as const;
}
function channel(value: Awaited<ReturnType<typeof cold>>) {
  if ("error" in value) throw new Error(value.error);
  return value;
}
async function traffic(f: Awaited<ReturnType<typeof network>>) {
  const path = `/networks/${f.network}/traffic`;
  const socket = await auth(path, (nonce) =>
    f.manager.proof(path, nonce, true),
  );
  expect((await socket.json()).type).toBe("traffic_ready");
  socket.send({
    type: "traffic_query",
    query: { period: "all", offset: 0, limit: 50 },
  });
  const result = await socket.json();
  socket.close();
  return result.report;
}
async function frame(source: Socket, target: Socket, length = 1024) {
  const bytes = new Uint8Array(length).fill(73);
  source.send(bytes.buffer);
  expect(new Uint8Array((await target.next()) as ArrayBuffer)).toEqual(bytes);
  target.send({ type: "ack", bytes: length });
  expect(await source.json()).toEqual({ type: "ack", bytes: length });
}
test("data tickets are network-bound, consumed once, and data frames bypass the coordinator SQL", async () => {
  const f = await network();
  const c = channel(await cold(f));
  expect((await response(c.route)).status).toBe(403);
  expect(
    (await response(c.route.replace(f.network, `net_${"b".repeat(52)}`)))
      .status,
  ).toBe(403);
  await frame(c.source, c.target); // Arms exactly one earliest checkpoint alarm.
  const root = await inspect("NETWORKS", RELAY_OBJECT);
  const data = await inspect("DATA", c.sid);
  expect(root.slots.filter((slot: any) => !slot.closed).length).toBe(1);
  expect(
    root.sockets.every(
      (socket: any) => socket.role !== "source" && socket.role !== "target",
    ),
  ).toBe(true);
  for (let i = 0; i < 8; i++) await frame(c.source, c.target);
  expect((await inspect("NETWORKS", RELAY_OBJECT)).changes).toBe(root.changes);
  expect((await inspect("DATA", c.sid)).changes).toBe(data.changes);
  const first = await traffic(f);
  expect(first.totals.ingress_bytes).toBe(9 * 1024);
  expect(first.totals.egress_bytes).toBe(9 * 1024);
  expect((await traffic(f)).totals).toEqual(first.totals); // Cumulative snapshots never count twice.
  c.source.close();
  c.target.close();
});
test("preconnected targets survive restoration and still use data shards without an incoming callback", async () => {
  const f = await network();
  const path = `/networks/${f.network}/standby/${f.target.device}/${f.controlled.generation}`;
  const registration = await auth(path, (nonce) => f.target.proof(path, nonce));
  const route = await registration.json();
  const target = await open(route.path);
  expect((await target.json()).type).toBe("standby_ready");
  const slot = (await inspect("NETWORKS", RELAY_OBJECT)).slots[0];
  await inspect("DATA", slot.sid, "restore");
  await inspect("NETWORKS", RELAY_OBJECT, "restore");
  const connectPath = `/networks/${f.network}/connect/${f.target.device}`;
  const registration2 = await auth(connectPath, (nonce) =>
    f.source.proof(connectPath, nonce),
  );
  const sourceRoute = await registration2.json();
  const source = await open(sourceRoute.path);
  expect((await target.json()).type).toBe("connected");
  expect((await source.json()).type).toBe("connected");
  await inspect("DATA", slot.sid, "restore");
  await frame(source, target, 64 * 1024);
  expect(
    (await inspect("NETWORKS", RELAY_OBJECT)).slots.filter(
      (slot: any) => !slot.closed && !slot.warm,
    ).length,
  ).toBe(1);
  const reader = f.controlled.socket
    .json()
    .then((value) => ({ unexpected: value }));
  const quiet = await Promise.race([
    reader,
    new Promise((resolve) => setTimeout(() => resolve("quiet"), 50)),
  ]);
  expect(quiet).toBe("quiet");
  source.close();
  target.close();
  f.controlled.socket.close();
  await reader.catch(() => {});
});
test("shard admission enforces source fairness and reserves the eighth slot for management", async () => {
  const f = await network();
  const sessions = [];
  for (let i = 0; i < SESSIONS_PER_SOURCE; i++)
    sessions.push(channel(await cold(f)));
  expect(await cold(f)).toEqual({ error: "SESSION_LIMIT" });
  const second = await f.member();
  for (let i = SESSIONS_PER_SOURCE; i < 7; i++)
    sessions.push(channel(await cold(f, second)));
  expect(await cold(f, second)).toEqual({ error: "SESSION_LIMIT" });
  sessions.push(channel(await cold(f, f.manager)));
  expect(await cold(f, f.manager)).toEqual({ error: "SESSION_LIMIT" });
  for (const c of sessions) {
    c.source.close();
    c.target.close();
  }
});
test("invalid restored bindings close both legs and keep the traffic report explicitly incomplete", async () => {
  const f = await network();
  const c = channel(await cold(f));
  await frame(c.source, c.target);
  await inspect("DATA", c.sid, "corrupt");
  await expect(c.target.next()).rejects.toThrow("closed");
  expect((await traffic(f)).complete).toBe(false);
});
test("placement hints accept the documented regions and reject arbitrary strings", () => {
  expect(region("apac-ne")).toBe("apac-ne");
  expect(region("arbitrary")).toBeUndefined();
  expect(edgeRegion("AS")).toBe("apac");
  expect(edgeRegion("XX")).toBeUndefined();
});

test("root-signed revocation closes the affected data object and the retired tickets", async () => {
  const f = await network();
  const managerControl = await f.control(f.manager);
  const c = channel(await cold(f));
  await frame(c.source, c.target);
  f.revoke(f.source.device);
  managerControl.socket.send({ type: "authority", state: await f.authority() });
  expect((await managerControl.socket.json()).type).toBe("authority_ack");
  await expect(c.source.next()).rejects.toThrow("closed");
  await expect(c.target.next()).rejects.toThrow("closed");
  expect((await response(c.route)).status).toBe(403);
  expect((await traffic(f)).totals).toEqual({
    ingress_bytes: 1024,
    egress_bytes: 1024,
  });
});
test("replacing target control closes its old data generation and permits a fresh channel", async () => {
  const f = await network();
  const c = channel(await cold(f));
  const replacement = await f.control(f.target);
  await expect(c.source.next()).rejects.toThrow("closed");
  await expect(c.target.next()).rejects.toThrow("closed");
  f.controlled = replacement;
  const next = channel(await cold(f));
  await frame(next.source, next.target);
  next.source.close();
  next.target.close();
});
test("restored data windows retain unacknowledged bytes until the receiver actually consumes them", async () => {
  const f = await network();
  const c = channel(await cold(f));
  c.source.send(new Uint8Array(8192).buffer);
  expect(((await c.target.next()) as ArrayBuffer).byteLength).toBe(8192);
  await inspect("DATA", c.sid, "restore");
  const state = await inspect("DATA", c.sid);
  expect(
    state.sockets.find((socket: any) => socket.side === "source").outstanding,
  ).toBe(8192);
  c.target.send({ type: "ack", bytes: 8192 });
  expect(await c.source.json()).toEqual({ type: "ack", bytes: 8192 });
  c.source.close();
  c.target.close();
});

test("completed data objects release their stored authority after final accounting", async () => {
  const f = await network();
  const c = channel(await cold(f));
  await frame(c.source, c.target);
  c.source.close();
  await expect(c.target.next()).rejects.toThrow("closed");
  const deadline = Date.now() + 1000;
  for (;;) {
    const slot = (await inspect("NETWORKS", RELAY_OBJECT)).slots.find(
      (slot: any) => slot.sid === c.sid,
    );
    if (slot.accounted) break;
    if (Date.now() >= deadline)
      throw new Error("Final checkpoint not acknowledged");
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  expect((await inspect("DATA", c.sid)).storedRows).toBe(0);
  expect((await traffic(f)).totals).toEqual({
    ingress_bytes: 1024,
    egress_bytes: 1024,
  });
});
