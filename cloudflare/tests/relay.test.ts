import { PROTOCOL, PROTOCOL_HEADER } from "../src/protocol";
import { Socket } from "./socket";
import { upgradeResponse } from "./upgrade-response";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  expect,
  test,
} from "bun:test";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { randomRoute } from "../src/auth";
import { fixture as makeFixture, VERSION } from "./fixtures";
import { RELAY_OBJECT } from "../src/routes";
const secret = randomRoute();
// Alarm delivery and individual socket reads each have a 5-second deadline.
// The containing regression needs room to finish both and report their errors.
const ALARM_TEST_TIMEOUT = 15_000;
let runtime: Miniflare;
let origin: string;
let bundleSource: string;
let gate: ReturnType<typeof Bun.serve>;
const heldProofs: ((response: Response) => void)[] = [];
beforeAll(async () => {
  gate = Bun.serve({
    port: 0,
    fetch(request) {
      if (new URL(request.url).pathname === "/release") {
        for (const resolve of heldProofs.splice(0))
          resolve(new Response("released"));
        return new Response("released");
      }
      return new Promise<Response>((resolve) => {
        heldProofs.push(resolve);
      });
    },
  });
  const bundle = await Bun.build({
    entrypoints: [`${import.meta.dir}/workerd/relay-worker.ts`],
    target: "browser",
    external: ["cloudflare:workers"],
  });
  if (!bundle.success)
    throw new AggregateError(bundle.logs, "Could not build relay test fixture");
  bundleSource = await bundle.outputs[0].text();
});
beforeEach(async () => {
  runtime = new Miniflare(
    convertV4MiniflareOptions({
      modules: true,
      script: bundleSource,
      compatibilityDate: "2026-10-03",
      durableObjects: { NETWORKS: { className: "TestRelay", useSQLite: true } },
      bindings: {
        RELAY_ROUTE: secret,
        XRUN_PROTOCOL_MIN: PROTOCOL.min,
        XRUN_PROTOCOL_MAX: PROTOCOL.max,
        XRUN_TEST_PROOF_GATE: gate.url.origin,
      },
    }),
  );
  origin = (await runtime.ready).origin;
});
afterEach(async () => {
  for (const resolve of heldProofs.splice(0)) resolve(new Response("released"));
  await runtime?.dispose();
});
afterAll(() => {
  gate?.stop(true);
});
async function open(path: string): Promise<Socket> {
  const response = await runtime.dispatchFetch(`${origin}/${secret}${path}`, {
    headers: {
      Upgrade: "websocket",
      "X-Xrun-Version": VERSION,
      "X-Xrun-Protocol": PROTOCOL_HEADER,
    },
  });
  if (!response.webSocket)
    throw new Error(`Upgrade ${response.status}: ${await response.text()}`);
  expect(response.headers.get("X-Xrun-Protocol")).toBe(String(PROTOCOL.max));
  return new Socket(response.webSocket);
}
async function authenticated(
  path: string,
  proof: (nonce: string) => Promise<unknown>,
) {
  const socket = await open(path);
  const challenge = await socket.json();
  expect(challenge.type).toBe("challenge");
  socket.send({
    type: "authenticate",
    proof: await proof(challenge.nonce),
    diagnostic: "optional",
  });
  return socket;
}

async function inspect(_network: string, path: string, body: object = {}) {
  const namespace = await runtime.getDurableObjectNamespace("NETWORKS");
  const object = namespace.get(namespace.idFromName(RELAY_OBJECT));
  return (
    await object.fetch(`https://test/test/${path}`, {
      method: "POST",
      body: JSON.stringify(body),
    })
  ).json();
}

async function fixture() {
  const network = await makeFixture();
  const manager = await network.member();
  const path = `/networks/${network.network}/claim`;
  const socket = await authenticated(path, (nonce) =>
    manager.proof(path, nonce, true),
  );
  expect(await socket.json()).toEqual({
    type: "relay_info",
    owner: { network_id: network.network, manager_id: manager.device },
  });
  socket.close();
  return { ...network, manager };
}

async function relayInfo() {
  const socket = await open("/networks/probe/status");
  expect((await socket.json()).type).toBe("challenge");
  const info = await socket.json();
  socket.close();
  return info;
}

test("short pairing locators route only to the proven manager and survive restoration", async () => {
  const f = await fixture();
  const path = `/networks/${f.network}/control`;
  const control = await authenticated(path, (nonce) =>
    f.manager.proof(path, nonce, true),
  );
  const hello = await control.json();
  const invitation_id = "a".repeat(32);
  control.send({
    type: "pairing_register",
    invitation_id,
    locator: "K7",
    expires_at: Date.now() + 600_000,
  });
  expect(await control.json()).toEqual({
    type: "pairing_registered",
    invitation_id,
    error: null,
  });
  await inspect(f.network, "restore");
  const response = await runtime.dispatchFetch(
    `${origin}/.well-known/xrun/pair/K7`,
    { headers: { Upgrade: "websocket", "X-Xrun-Protocol": PROTOCOL_HEADER } },
  );
  expect(response.status).toBe(101);
  const source = new Socket(response.webSocket!);
  expect(await source.json()).toEqual({
    type: "pairing_info",
    invitation_id,
    locator: "K7",
  });
  expect((await source.json()).type).toBe("challenge");
  source.send({ type: "authenticate", proof: null });
  const incoming = await control.json();
  expect(incoming.type).toBe("pairing_incoming");
  expect(incoming.invitation_id).toBe(invitation_id);
  const target = await open(
    `/networks/${f.network}/attach/${f.manager.device}/${hello.generation}/${incoming.session_id}`,
  );
  expect((await source.json()).type).toBe("connected");
  expect((await target.json()).type).toBe("connected");
  await inspect(f.network, "restore");
  source.send(new Uint8Array([1, 2, 3]).buffer);
  expect(new Uint8Array((await target.next()) as ArrayBuffer)).toEqual(
    new Uint8Array([1, 2, 3]),
  );
  source.close();
  target.close();
  control.send({ type: "pairing_cancel", invitation_id });
  expect((await control.json()).error).toBeNull();
  const missing = await upgradeResponse(`${origin}/.well-known/xrun/pair/K7`, {
    "X-Xrun-Protocol": PROTOCOL_HEADER,
  });
  expect(missing.status).toBe(404);
  expect(await missing.json()).toMatchObject({
    type: "error",
    code: "INVALID_CODE",
  });
  control.close();
});

test("short pairing registration rejects ordinary members and duplicate or expired locators", async () => {
  const f = await fixture();
  const path = `/networks/${f.network}/control`;
  const control = await authenticated(path, (nonce) =>
    f.manager.proof(path, nonce, true),
  );
  await control.json();
  control.send({
    type: "pairing_register",
    invitation_id: "a".repeat(32),
    locator: "K7",
    expires_at: Date.now() + 600_000,
  });
  expect((await control.json()).error).toBeNull();
  control.send({
    type: "pairing_register",
    invitation_id: "b".repeat(32),
    locator: "K7",
    expires_at: Date.now() + 600_000,
  });
  expect((await control.json()).error.code).toBe("PAIRING_BUSY");
  control.send({
    type: "pairing_register",
    invitation_id: "c".repeat(32),
    locator: "M4",
    expires_at: Date.now() - 1,
  });
  expect((await control.json()).error.code).toBe("INVALID_REQUEST");
  const member = await f.member();
  const ordinary = await authenticated(path, (nonce) =>
    member.proof(path, nonce),
  );
  await ordinary.json();
  ordinary.send({
    type: "pairing_register",
    invitation_id: "d".repeat(32),
    locator: "M4",
    expires_at: Date.now() + 600_000,
  });
  expect((await ordinary.json()).code).toBe("NOT_MANAGER");
  ordinary.close();
  control.close();
});

test("probing, anonymous clients and invalid manager proofs cannot claim an unused relay", async () => {
  const f = await makeFixture();
  const member = await f.member();
  const path = `/networks/${f.network}/claim`;
  expect(await relayInfo()).toEqual({ type: "relay_info", owner: null });
  const controlPath = `/networks/${f.network}/control`;
  const ordinary = await authenticated(controlPath, (nonce) =>
    member.proof(controlPath, nonce),
  );
  expect((await ordinary.json()).code).toBe("RELAY_UNCLAIMED");
  ordinary.close();
  for (const [proof, code] of [
    [async () => null, "NOT_MANAGER"],
    [(nonce: string) => member.proof(path, nonce), "NOT_MANAGER"],
    [async () => member.proof(path, "replayed", true), "UNAUTHENTICATED"],
  ] as const) {
    const socket = await authenticated(path, proof);
    expect((await socket.json()).code).toBe(code);
    socket.close();
    expect((await relayInfo()).owner).toBeNull();
  }
});

test("concurrent valid claims choose one persistent manager and reject every foreign network route", async () => {
  const networks = await Promise.all([makeFixture(), makeFixture()]);
  const managers = await Promise.all(networks.map((f) => f.member()));
  const paths = networks.map((f) => `/networks/${f.network}/claim`);
  // Open both challenges before either claim is submitted.
  const sockets = await Promise.all(paths.map(open));
  const challenges = await Promise.all(sockets.map((socket) => socket.json()));
  const proofs = await Promise.all(
    managers.map((member, index) =>
      member.proof(paths[index], challenges[index].nonce, true),
    ),
  );
  sockets.forEach((socket, index) =>
    socket.send({ type: "authenticate", proof: proofs[index] }),
  );
  const replies = await Promise.all(sockets.map((socket) => socket.json()));
  const winner = replies.findIndex((reply) => reply.type === "relay_info");
  expect(winner).not.toBe(-1);
  expect(replies[1 - winner].code).toBe("RELAY_CLAIMED");
  const owner = {
    network_id: networks[winner].network,
    manager_id: managers[winner].device,
  };
  expect(replies[winner].owner).toEqual(owner);
  expect((await relayInfo()).owner).toEqual(owner);
  await inspect(owner.network_id, "restore");
  expect((await relayInfo()).owner).toEqual(owner);
  const retry = await authenticated(paths[winner], (nonce) =>
    managers[winner].proof(paths[winner], nonce, true),
  );
  expect((await retry.json()).owner).toEqual(owner);
  retry.close();

  const foreign = networks[1 - winner].network;
  const device = managers[1 - winner].device;
  for (const action of [
    "claim",
    "status",
    "traffic",
    "control",
    `connect/${device}`,
    `attach/${device}/${"a".repeat(32)}/${"b".repeat(32)}`,
  ]) {
    const response = await upgradeResponse(
      `${origin}/${secret}/networks/${foreign}/${action}`,
      { "X-Xrun-Version": VERSION, "X-Xrun-Protocol": PROTOCOL_HEADER },
    );
    expect(response.status).toBe(403);
    expect(await response.json()).toMatchObject({
      type: "error",
      code: "RELAY_CLAIMED",
    });
  }
  const member = await networks[winner].member();
  const claimedPath = paths[winner];
  const takeover = await authenticated(claimedPath, (nonce) =>
    member.proof(claimedPath, nonce, true),
  );
  expect((await takeover.json()).code).toBe("RELAY_CLAIMED");
  const controlPath = `/networks/${owner.network_id}/control`;
  const invalidManager = await authenticated(controlPath, (nonce) =>
    member.proof(controlPath, nonce, true),
  );
  expect((await invalidManager.json()).code).toBe("NOT_MANAGER");
  const ordinary = await authenticated(controlPath, (nonce) =>
    member.proof(controlPath, nonce),
  );
  expect((await ordinary.json()).type).toBe("hello_ack");
  for (const socket of [...sockets, takeover, invalidManager, ordinary])
    socket.close();
});

test("an existing manager can claim on reconnect while ordinary members cannot", async () => {
  const f = await makeFixture();
  const member = await f.member();
  const manager = await f.member();
  const path = `/networks/${f.network}/control`;
  const ordinary = await authenticated(path, (nonce) =>
    member.proof(path, nonce),
  );
  expect((await ordinary.json()).code).toBe("RELAY_UNCLAIMED");
  expect((await relayInfo()).owner).toBeNull();
  const control = await authenticated(path, (nonce) =>
    manager.proof(path, nonce, true),
  );
  expect((await control.json()).type).toBe("hello_ack");
  expect((await relayInfo()).owner).toEqual({
    network_id: f.network,
    manager_id: manager.device,
  });
  const admitted = await authenticated(path, (nonce) =>
    member.proof(path, nonce),
  );
  expect((await admitted.json()).type).toBe("hello_ack");
  for (const socket of [ordinary, control, admitted]) socket.close();
});

async function pair(existing?: Awaited<ReturnType<typeof fixture>>) {
  const network = existing ?? (await fixture());
  const target = await network.member(),
    source = network.manager;
  const path = `/networks/${network.network}/control`;
  const control = await authenticated(path, (nonce) =>
    target.proof(path, nonce),
  );
  const hello = await control.json();
  const connect = `/networks/${network.network}/connect/${target.device}`;
  const sender = await authenticated(connect, (nonce) =>
    source.proof(connect, nonce),
  );
  const incoming = await control.json();
  const attach = `/networks/${network.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`;
  const receiver = await open(attach);
  await sender.json();
  await receiver.json();
  return {
    fixture: network,
    source,
    target,
    network: network.network,
    control,
    sender,
    receiver,
    attach,
    sid: incoming.session_id,
  };
}

test("traffic persists across socket restoration and closure with authenticated network/device scope", async () => {
  const p = await pair();
  const query = async (
    member: typeof p.source,
    manager = false,
    offset = 0,
    limit = 50,
  ) => {
    const path = "/networks/" + p.network + "/traffic";
    const socket = await authenticated(path, (nonce) =>
      member.proof(path, nonce, manager),
    );
    expect(await socket.json()).toEqual({ type: "traffic_ready" });
    socket.send({
      type: "traffic_query",
      query: { period: "all", offset, limit },
    });
    const response = await socket.json();
    expect(response.type).toBe("traffic");
    return response.report;
  };
  p.sender.send(new Uint8Array(3072).buffer);
  expect(((await p.receiver.next()) as ArrayBuffer).byteLength).toBe(3072);
  p.receiver.send(new Uint8Array(1024).buffer);
  expect(((await p.sender.next()) as ArrayBuffer).byteLength).toBe(1024);
  p.receiver.send({ type: "ack", bytes: 3072 });
  expect(await p.sender.json()).toEqual({ type: "ack", bytes: 3072 });
  p.sender.send({ type: "cache_state", idle: true });
  expect(await p.sender.json()).toEqual({ type: "cache_state", idle: true });
  await inspect(p.network, "restore");
  p.sender.send({ type: "cache_state", idle: false });
  expect(await p.sender.json()).toEqual({ type: "cache_state", idle: false });
  p.sender.send(new Uint8Array(128).buffer);
  await p.receiver.next();
  const source = await query(p.source);
  expect(source.device_id).toBe(p.source.device);
  expect(source.totals).toEqual({ ingress_bytes: 3200, egress_bytes: 1024 });
  expect(source.devices).toEqual([
    { device_id: p.source.device, sent_bytes: 3200, received_bytes: 1024 },
  ]);
  const target = await query(p.target);
  expect(target.totals).toEqual({ ingress_bytes: 1024, egress_bytes: 3200 });
  const report = await query(p.source, true);
  expect(report.device_id).toBeNull();
  expect(report.complete).toBe(true);
  expect(report.totals).toEqual({ ingress_bytes: 4224, egress_bytes: 4224 });
  expect(report.daily).toHaveLength(30);
  expect(report.daily.at(-1)).toMatchObject(report.totals);
  const first = await query(p.source, true, 0, 1);
  expect(first.next_offset).toBe(1);
  const second = await query(p.source, true, 1, 1);
  expect(second.next_offset).toBeNull();
  expect(first.devices[0].device_id).not.toBe(second.devices[0].device_id);
  p.sender.close();
  p.receiver.close();
  p.control.close();
  expect((await query(p.source, true)).totals).toEqual(report.totals);
  const anonymous = await authenticated(
    "/networks/" + p.network + "/traffic",
    async () => null,
  );
  expect((await anonymous.json()).code).toBe("UNAUTHENTICATED");
});

test("cached sessions survive restoration and yield capacity while active sessions stay protected", async () => {
  const network = await fixture();
  const target = await network.member(),
    source = await network.member();
  const path = `/networks/${network.network}/control`;
  const control = await authenticated(path, (nonce) =>
    target.proof(path, nonce),
  );
  const hello = await control.json();
  const connect = `/networks/${network.network}/connect/${target.device}`;
  async function session() {
    const sender = await authenticated(connect, (nonce) =>
      source.proof(connect, nonce),
    );
    const incoming = await control.json();
    const receiver = await open(
      `/networks/${network.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`,
    );
    expect((await sender.json()).type).toBe("connected");
    expect((await receiver.json()).type).toBe("connected");
    return { sender, receiver };
  }
  const cached = [];
  for (let index = 0; index < 6; index++) {
    const connection = await session();
    connection.sender.send({ type: "cache_state", idle: true });
    expect(await connection.sender.json()).toEqual({
      type: "cache_state",
      idle: true,
    });
    cached.push(connection);
  }
  await inspect(network.network, "restore");
  const active = [];
  for (let index = 0; index < 4; index++) active.push(await session());
  for (const connection of cached) {
    await expect(connection.sender.next()).rejects.toThrow("closed");
    await expect(connection.receiver.next()).rejects.toThrow("closed");
  }
  // Restoring and reactivating a cache removes its eviction eligibility.
  active[0].sender.send({ type: "cache_state", idle: true });
  await active[0].sender.json();
  await inspect(network.network, "restore");
  active[0].sender.send({ type: "cache_state", idle: false });
  expect(await active[0].sender.json()).toEqual({
    type: "cache_state",
    idle: false,
  });
  const rejected = await authenticated(connect, (nonce) =>
    source.proof(connect, nonce),
  );
  expect((await rejected.json()).code).toBe("SESSION_LIMIT");
  const frame = new Uint8Array([1, 2, 3]).buffer;
  for (const connection of active) {
    connection.sender.send(frame);
    expect(
      new Uint8Array((await connection.receiver.next()) as ArrayBuffer),
    ).toEqual(new Uint8Array(frame));
    connection.sender.close();
    connection.receiver.close();
  }
  rejected.close();
  control.close();
}, 15000);

test("restoration preserves ciphertext credit and one-time attach, and frame processing stays indexed", async () => {
  const { network, control, sender, receiver, attach } = await pair();
  const frame = new Uint8Array(64 * 1024).buffer;
  sender.send(frame);
  await receiver.next();
  await inspect(network, "restore");
  await expect(open(attach)).rejects.toThrow("Upgrade 400");
  receiver.send({ type: "ack", bytes: frame.byteLength });
  expect(await sender.json()).toEqual({ type: "ack", bytes: frame.byteLength });
  const before = await inspect(network, "metrics");
  const started = performance.now();
  for (let i = 0; i < 500; i++) {
    sender.send(frame);
    expect(((await receiver.next()) as ArrayBuffer).byteLength).toBe(
      frame.byteLength,
    );
    receiver.send({ type: "ack", bytes: frame.byteLength });
    expect((await sender.json()).bytes).toBe(frame.byteLength);
  }
  expect(await inspect(network, "metrics")).toEqual(before);
  console.info(
    `M3 indexed relay: 500 frames and acknowledgements in ${(performance.now() - started).toFixed(1)} ms; no socket enumeration or alarm reads`,
  );
  for (let i = 0; i < 64; i++) {
    sender.send(frame);
    await receiver.next();
  }
  await inspect(network, "restore");
  sender.send(new Uint8Array([1]).buffer);
  expect((await sender.json()).code).toBe("MESSAGE_TOO_LARGE");
  await expect(receiver.next()).rejects.toThrow("closed");
  control.close();
});

test("source fairness survives restoration and the manager retains the final network slot", async () => {
  const f = await fixture();
  const manager = f.manager;
  const first = await f.member();
  const neighbor = await f.member();
  const controlPath = `/networks/${f.network}/control`;
  const control = await authenticated(controlPath, (nonce) =>
    manager.proof(controlPath, nonce, true),
  );
  const generation = (await control.json()).generation;
  const connect = `/networks/${f.network}/connect/${manager.device}`;
  const sockets: Socket[] = [control];
  async function session(member: typeof first) {
    const source = await authenticated(connect, (nonce) =>
      member.proof(connect, nonce),
    );
    sockets.push(source);
    const incoming = await control.json();
    const target = await open(
      `/networks/${f.network}/attach/${manager.device}/${generation}/${incoming.session_id}`,
    );
    sockets.push(target);
    expect((await source.json()).type).toBe("connected");
    expect((await target.json()).type).toBe("connected");
    return source;
  }
  try {
    for (let i = 0; i < 4; i++) await session(first);
    await inspect(f.network, "restore");
    const fifth = await authenticated(connect, (nonce) =>
      first.proof(connect, nonce),
    );
    sockets.push(fifth);
    expect((await fifth.json()).code).toBe("SESSION_LIMIT");
    // A different authenticated device behind the same IP has its own budget.
    for (let i = 0; i < 3; i++) await session(neighbor);
    const ordinary = await authenticated(connect, (nonce) =>
      neighbor.proof(connect, nonce),
    );
    sockets.push(ordinary);
    expect((await ordinary.json()).code).toBe("SESSION_LIMIT");
    const anonymous = await authenticated(connect, async () => null);
    sockets.push(anonymous);
    expect((await anonymous.json()).code).toBe("SESSION_LIMIT");
    // Source proofs need no new wire fields: the manager's control proved the root key.
    await session(manager);
    await inspect(f.network, "restore");
    const ninth = await authenticated(connect, (nonce) =>
      manager.proof(connect, nonce),
    );
    sockets.push(ninth);
    expect((await ninth.json()).code).toBe("SESSION_LIMIT");
  } finally {
    for (const socket of sockets) socket.close();
  }
});

test("invalid restored role fields fail closed and simultaneous peer expiry clears the shared alarm", async () => {
  const f = await fixture();
  const { network, control, sender, receiver, sid } = await pair(f);
  await inspect(network, "restore", { sid, role: "source" });
  await expect(sender.next()).rejects.toThrow("closed");
  await expect(receiver.next()).rejects.toThrow("closed");
  control.close();
  const fresh = await pair(f);
  await alarm(fresh.network, { sid: fresh.sid });
  const notices = await Promise.allSettled([
    fresh.sender.json(),
    fresh.receiver.json(),
  ]);
  const codes = notices.flatMap((notice) =>
    notice.status === "fulfilled" ? [notice.value.code] : [],
  );
  expect(codes).toEqual(["CONNECT_TIMEOUT"]);
  await expect(fresh.sender.next()).rejects.toThrow("closed");
  await expect(fresh.receiver.next()).rejects.toThrow("closed");
  expect(
    ((await inspect(fresh.network, "state")) as { alarm: number | null }).alarm,
  ).toBeNull();
  fresh.control.close();
}, ALARM_TEST_TIMEOUT);

test("a delayed valid proof cannot register a control after its verification lease expires", async () => {
  const network = await fixture(),
    member = await network.member();
  await inspect(network.network, "pause");
  const path = `/networks/${network.network}/control`;
  const delayed = await authenticated(path, (nonce) =>
    member.proof(path, nonce),
  );
  const readyBy = Date.now() + 5000;
  while (!heldProofs.length) {
    if (Date.now() >= readyBy)
      throw new Error(
        "Proof did not reach the controlled verification barrier",
      );
    await Bun.sleep(5);
  }
  await alarm(network.network, { role: "verifying" });
  expect((await delayed.json()).code).toBe("CONNECT_TIMEOUT");
  await expect(delayed.next()).rejects.toThrow("closed");
  await inspect(network.network, "release");
  const statusPath = `/networks/${network.network}/status`;
  const status = await authenticated(statusPath, (nonce) =>
    member.proof(statusPath, nonce),
  );
  expect((await status.json()).devices).toEqual([]);
  const valid = await authenticated(path, (nonce) => member.proof(path, nonce));
  expect((await valid.json()).type).toBe("hello_ack");
  valid.close();
  status.close();
}, ALARM_TEST_TIMEOUT);

test("idle authentication sockets are capped and client headers cannot bypass the cap", async () => {
  const f = await fixture();
  const path = `/networks/${f.network}/status`;
  const waiting: Socket[] = [];
  for (let i = 0; i < 8; i++) {
    const socket = await open(path);
    expect((await socket.json()).type).toBe("challenge");
    waiting.push(socket);
  }
  const response = await runtime.dispatchFetch(`${origin}/${secret}${path}`, {
    headers: {
      Upgrade: "websocket",
      "X-Xrun-Version": VERSION,
      "X-Xrun-Protocol": PROTOCOL_HEADER,
      "X-Xrun-Peer": "forged",
    },
  });
  expect(response.status).toBe(429);
  expect(response.webSocket).toBeNull();
  for (const socket of waiting) socket.close();
});

// Runs production relay handlers in workerd; only alarm controls are test-specific.
test("secret route, protocol and proof protect existing device routes", async () => {
  expect(
    (await runtime.dispatchFetch(`${origin}/wrong/networks/probe/status`))
      .status,
  ).toBe(404);
  expect(
    (
      await runtime.dispatchFetch(`${origin}/${secret}/networks/probe/status`, {
        headers: {
          Upgrade: "websocket",
          "X-Xrun-Version": VERSION,
          "X-Xrun-Protocol": `${PROTOCOL.max + 1}-${PROTOCOL.max + 1}`,
        },
      })
    ).status,
  ).toBe(409);
  const f = await fixture();
  const target = await f.member(),
    source = await f.member();
  const path = `/networks/${f.network}/control`;
  const control = await authenticated(path, (nonce) =>
    target.proof(path, nonce),
  );
  const hello = await control.json();
  expect(hello.type).toBe("hello_ack");
  for (const make of [
    async (nonce: string) => ({
      ...(await source.proof(path, nonce)),
      device_id: target.device,
    }),
    async (nonce: string) => ({
      ...(await target.proof(path, nonce)),
      signature: (await source.proof(path, nonce)).signature,
    }),
    async () => target.proof(path, "replay"),
    async () => null,
  ]) {
    const attacker = await authenticated(path, make);
    expect((await attacker.json()).code).toBe("UNAUTHENTICATED");
    attacker.close();
  }
  const connect = `/networks/${f.network}/connect/${target.device}`;
  const anonymous = await authenticated(connect, async () => null);
  expect((await anonymous.json()).code).toBe("UNAUTHENTICATED");
  const sender = await authenticated(connect, (nonce) =>
    source.proof(connect, nonce),
  );
  const incoming = await control.json();
  expect(incoming.type).toBe("incoming");
  const attach = `/networks/${f.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`;
  const receiver = await open(attach);
  expect(await receiver.json()).toEqual({
    type: "connected",
    flow_control: true,
  });
  expect(await sender.json()).toEqual({
    type: "connected",
    flow_control: true,
  });
  await expect(open(attach)).rejects.toThrow("Upgrade 400");
  // Acknowledgements release the window without limiting cumulative transfer.
  for (let i = 0; i < 140; i++) {
    const bytes = new Uint8Array(64 * 1024).fill(i % 256).buffer;
    sender.send(bytes);
    expect(new Uint8Array((await receiver.next()) as ArrayBuffer)).toEqual(
      new Uint8Array(bytes),
    );
    receiver.send({ type: "ack", bytes: bytes.byteLength });
    expect(await sender.json()).toEqual({
      type: "ack",
      bytes: bytes.byteLength,
    });
  }
  const replacement = await authenticated(path, (nonce) =>
    target.proof(path, nonce),
  );
  expect((await replacement.json()).generation).not.toBe(hello.generation);
  await expect(sender.next()).rejects.toThrow("closed");
  await expect(open(attach)).rejects.toThrow("Upgrade 400");
  for (const socket of [control, anonymous, sender, receiver, replacement])
    socket.close();
}, 20_000);

test("anonymous pairing is manager-only, limited, and cleaned on control disconnect", async () => {
  const f = await fixture(),
    manager = f.manager;
  const path = `/networks/${f.network}/control`;
  const control = await authenticated(path, (nonce) =>
    manager.proof(path, nonce, true),
  );
  await control.json();
  const connect = `/networks/${f.network}/connect/${manager.device}`;
  const pending: Socket[] = [];
  for (let i = 0; i < 4; i++) {
    pending.push(await authenticated(connect, async () => null));
    expect((await control.json()).type).toBe("incoming");
  }
  const overflow = await authenticated(connect, async () => null);
  expect((await overflow.json()).code).toBe("SESSION_LIMIT");
  control.close();
  for (const socket of pending) {
    await expect(socket.next()).rejects.toThrow("closed");
    socket.close();
  }
  overflow.close();
});

test("unacknowledged ciphertext is bounded and forged credit closes both peers", async () => {
  const f = await fixture();
  for (const forged of [false, true]) {
    const target = await f.member(),
      source = await f.member();
    const path = `/networks/${f.network}/control`;
    const control = await authenticated(path, (nonce) =>
      target.proof(path, nonce),
    );
    const hello = await control.json();
    const connect = `/networks/${f.network}/connect/${target.device}`;
    const sender = await authenticated(connect, (nonce) =>
      source.proof(connect, nonce),
    );
    const incoming = await control.json();
    const receiver = await open(
      `/networks/${f.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`,
    );
    await receiver.json();
    await sender.json();
    if (forged) {
      receiver.send({ type: "ack", bytes: 1 });
      expect((await receiver.json()).code).toBe("INVALID_MESSAGE");
      await expect(sender.next()).rejects.toThrow("closed");
    } else {
      for (let i = 0; i < 64; i++) {
        sender.send(new Uint8Array(64 * 1024).buffer);
        await receiver.next();
      }
      sender.send(new Uint8Array(1).buffer);
      expect((await sender.json()).code).toBe("MESSAGE_TOO_LARGE");
      await expect(receiver.next()).rejects.toThrow("closed");
    }
    control.close();
    sender.close();
    receiver.close();
  }
});

test("pending and active sessions share the network buffer budget and release their slots", async () => {
  const f = await fixture(),
    source = await f.member();
  const other = await f.member();
  const targets = await Promise.all([f.member(), f.member()]);
  const controlPath = `/networks/${f.network}/control`;
  const controls: Socket[] = [],
    generations: string[] = [],
    sources: Socket[] = [],
    receivers: Socket[] = [];
  for (const target of targets) {
    const control = await authenticated(controlPath, (nonce) =>
      target.proof(controlPath, nonce),
    );
    generations.push((await control.json()).generation);
    controls.push(control);
  }
  for (let i = 0; i < 7; i++) {
    const index = i % 2,
      target = targets[index];
    const connect = `/networks/${f.network}/connect/${target.device}`;
    const sender = await authenticated(connect, (nonce) =>
      (i % 2 ? other : source).proof(connect, nonce),
    );
    sources.push(sender);
    const incoming = await controls[index].json();
    expect(incoming.type).toBe("incoming");
    if (i < 4) {
      const receiver = await open(
        `/networks/${f.network}/attach/${target.device}/${generations[index]}/${incoming.session_id}`,
      );
      expect((await receiver.json()).type).toBe("connected");
      expect((await sender.json()).type).toBe("connected");
      receivers.push(receiver);
    }
  }
  const connect = `/networks/${f.network}/connect/${targets[1].device}`;
  const overflow = await authenticated(connect, (nonce) =>
    source.proof(connect, nonce),
  );
  expect((await overflow.json()).code).toBe("SESSION_LIMIT");
  sources[0].close();
  await expect(receivers[0].next()).rejects.toThrow("closed");
  const replacement = await authenticated(connect, (nonce) =>
    source.proof(connect, nonce),
  );
  expect((await controls[1].json()).type).toBe("incoming");
  for (const socket of [
    ...controls,
    ...sources.slice(1),
    ...receivers,
    overflow,
    replacement,
  ])
    socket.close();
});

async function alarm(
  _network: string,
  expire: { role?: string; sid?: string } = {},
) {
  const namespace = await runtime.getDurableObjectNamespace("NETWORKS");
  const object = namespace.get(namespace.idFromName(RELAY_OBJECT));
  const previous = (await (
    await object.fetch("https://test/test/alarm", {
      method: "POST",
      body: JSON.stringify(expire),
    })
  ).json()) as { runs: number };
  const deadline = Date.now() + 5000;
  while (Date.now() < deadline) {
    const state = (await (
      await object.fetch("https://test/test/state")
    ).json()) as { runs: number; alarm: number | null };
    // The run marker commits before workerd finishes the alarm handler. Wait
    // until its firing timestamp has been cleared or replaced by a future alarm.
    if (
      state.runs > previous.runs &&
      (state.alarm === null || state.alarm > Date.now() + 1)
    )
      return state;
    await Bun.sleep(10);
  }
  throw new Error("workerd did not deliver the alarm");
}

test("authentication deadlines close idle clients, recover admission slots and preserve control", async () => {
  const f = await fixture(),
    member = await f.member();
  const controlPath = `/networks/${f.network}/control`;
  const control = await authenticated(controlPath, (nonce) =>
    member.proof(controlPath, nonce),
  );
  await control.json();
  const path = `/networks/${f.network}/status`;
  const waiting: Socket[] = [];
  for (let i = 0; i < 8; i++) {
    const socket = await open(path);
    expect((await socket.json()).type).toBe("challenge");
    waiting.push(socket);
  }
  await expect(open(path)).rejects.toThrow("Upgrade 429");
  const state = await alarm(f.network, { role: "auth" });
  expect(state.alarm).toBeNull();
  for (const socket of waiting) {
    expect((await socket.json()).code).toBe("CONNECT_TIMEOUT");
    await expect(socket.next()).rejects.toThrow("closed");
  }
  const status = await authenticated(path, (nonce) =>
    member.proof(path, nonce),
  );
  expect((await status.json()).devices).toEqual([member.device]);
  // All eight admissions, not just one, must be reusable after expiry.
  for (let i = 0; i < 8; i++) {
    const socket = await open(path);
    expect((await socket.json()).type).toBe("challenge");
    waiting.push(socket);
  }
  for (const socket of [...waiting, control, status]) socket.close();
}, ALARM_TEST_TIMEOUT);

test("pending callback deadlines invalidate attach URLs and release session capacity", async () => {
  const f = await fixture(),
    manager = f.manager;
  const path = `/networks/${f.network}/control`;
  const control = await authenticated(path, (nonce) =>
    manager.proof(path, nonce, true),
  );
  const hello = await control.json();
  const connect = `/networks/${f.network}/connect/${manager.device}`;
  const pending: Socket[] = [],
    stale: string[] = [];
  for (let i = 0; i < 4; i++) {
    pending.push(await authenticated(connect, async () => null));
    stale.push((await control.json()).session_id);
  }
  const overflow = await authenticated(connect, async () => null);
  expect((await overflow.json()).code).toBe("SESSION_LIMIT");
  expect((await alarm(f.network, { role: "pending" })).alarm).toBeNull();
  for (const socket of pending) {
    expect((await socket.json()).code).toBe("CONNECT_TIMEOUT");
    await expect(socket.next()).rejects.toThrow("closed");
  }
  for (const sid of stale)
    await expect(
      open(
        `/networks/${f.network}/attach/${manager.device}/${hello.generation}/${sid}`,
      ),
    ).rejects.toThrow("Upgrade 400");
  for (let i = 0; i < 4; i++) {
    pending.push(await authenticated(connect, async () => null));
    expect((await control.json()).type).toBe("incoming");
  }
  for (const socket of [...pending, overflow, control]) socket.close();
}, ALARM_TEST_TIMEOUT);

test("idle expiry closes both tunnel peers, preserves live traffic and restores the shared budget", async () => {
  const f = await fixture(),
    target = await f.member(),
    source = await f.member();
  const other = await f.member();
  const path = `/networks/${f.network}/control`;
  const control = await authenticated(path, (nonce) =>
    target.proof(path, nonce),
  );
  const hello = await control.json();
  const connect = `/networks/${f.network}/connect/${target.device}`;
  const sources: Socket[] = [],
    targets: Socket[] = [],
    sessions: string[] = [];
  for (let i = 0; i < 7; i++) {
    const sender = await authenticated(connect, (nonce) =>
      (i % 2 ? other : source).proof(connect, nonce),
    );
    const incoming = await control.json();
    const receiver = await open(
      `/networks/${f.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`,
    );
    await sender.json();
    await receiver.json();
    sources.push(sender);
    targets.push(receiver);
    sessions.push(incoming.session_id);
  }
  const overflow = await authenticated(connect, (nonce) =>
    source.proof(connect, nonce),
  );
  expect((await overflow.json()).code).toBe("SESSION_LIMIT");
  // A prematurely delivered alarm must leave all unexpired sessions intact.
  expect((await alarm(f.network)).alarm).toBeGreaterThan(Date.now());
  sources[0].send(new Uint8Array(64 * 1024).buffer);
  await targets[0].next(); // Leave ciphertext unacknowledged when this pair expires.
  const state = await alarm(f.network, { role: "source", sid: sessions[0] });
  expect(state.alarm).toBeGreaterThan(Date.now());
  expect((await sources[0].json()).code).toBe("CONNECT_TIMEOUT");
  await expect(sources[0].next()).rejects.toThrow("closed");
  await expect(targets[0].next()).rejects.toThrow("closed");
  const bytes = new Uint8Array([1, 2, 3]).buffer;
  for (let i = 1; i < 7; i++) {
    sources[i].send(bytes);
    expect(new Uint8Array((await targets[i].next()) as ArrayBuffer)).toEqual(
      new Uint8Array(bytes),
    );
    targets[i].send({ type: "ack", bytes: 3 });
    expect(await sources[i].json()).toEqual({ type: "ack", bytes: 3 });
  }
  const replacement = await authenticated(connect, (nonce) =>
    source.proof(connect, nonce),
  );
  expect((await control.json()).type).toBe("incoming");
  const full = await authenticated(connect, (nonce) =>
    source.proof(connect, nonce),
  );
  expect((await full.json()).code).toBe("SESSION_LIMIT");
  for (const socket of [
    ...sources,
    ...targets,
    control,
    overflow,
    replacement,
    full,
  ])
    socket.close();
}, ALARM_TEST_TIMEOUT);

test("signed revocation persists across restoration and closes both ciphertext legs", async () => {
  const f = await fixture();
  const target = await f.member(),
    source = await f.member();
  const controlPath = `/networks/${f.network}/control`;
  const manager = await authenticated(controlPath, (nonce) =>
    f.manager.proof(controlPath, nonce, true),
  );
  await manager.json();
  const control = await authenticated(controlPath, (nonce) =>
    target.proof(controlPath, nonce),
  );
  const hello = await control.json();
  const connect = `/networks/${f.network}/connect/${target.device}`;
  const sender = await authenticated(connect, (nonce) =>
    source.proof(connect, nonce),
  );
  const incoming = await control.json();
  const receiver = await open(
    `/networks/${f.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`,
  );
  await sender.json();
  await receiver.json();
  f.revoke(source.device);
  const state = await f.authority();
  manager.send({ type: "authority", state });
  expect(await manager.json()).toMatchObject({
    type: "authority_ack",
    version: state.roster.roster.version,
    hash: state.lease.lease.hash,
  });
  await expect(sender.next()).rejects.toThrow("closed");
  await expect(receiver.next()).rejects.toThrow("closed");
  await inspect(f.network, "restore");
  const refused = await authenticated(controlPath, (nonce) =>
    source.proof(controlPath, nonce),
  );
  expect((await refused.json()).code).toBe("DEVICE_REVOKED");
  const statusPath = `/networks/${f.network}/status`;
  const status = await authenticated(statusPath, (nonce) =>
    f.manager.proof(statusPath, nonce, true),
  );
  const pending = (await status.json()).pending_authority;
  expect(pending).not.toContain(target.device);
  expect(pending).not.toContain(source.device);
  for (const socket of [manager, control, refused, status]) socket.close();
});

test("management generations replace the owner and reject a restored obsolete root proof", async () => {
  const f = await fixture(),
    next = await f.member();
  const controlPath = `/networks/${f.network}/control`;
  const old = await authenticated(controlPath, (nonce) =>
    f.manager.proof(controlPath, nonce, true),
  );
  await old.json();
  const previous = await f.authority();
  f.migrate(next.device);
  const current = await authenticated(controlPath, (nonce) =>
    next.proof(controlPath, nonce, true),
  );
  expect((await current.json()).type).toBe("hello_ack");
  await expect(old.next()).rejects.toThrow("closed");
  expect((await relayInfo()).owner.manager_id).toBe(next.device);
  await inspect(f.network, "restore");
  current.send({ type: "authority", state: previous });
  expect((await current.json()).code).toBe("INVALID_MESSAGE");
  const obsolete = await authenticated(controlPath, (nonce) =>
    f.manager.proof(controlPath, nonce, true),
  );
  expect((await obsolete.json()).code).toBe("NOT_MANAGER");
  current.close();
  obsolete.close();
});

test("authenticated standby callbacks pair without a target notification and survive restoration", async () => {
  const f = await fixture();
  const leaf = await f.member();
  const targetPath = `/networks/${f.network}/control`;
  const target = await authenticated(targetPath, (nonce) =>
    leaf.proof(targetPath, nonce),
  );
  const hello = await target.json();
  expect(hello.type).toBe("hello_ack");
  const standbyPath = `/networks/${f.network}/standby/${leaf.device}/${hello.generation}`;
  const standby = await authenticated(standbyPath, (nonce) =>
    leaf.proof(standbyPath, nonce),
  );
  expect(await standby.json()).toEqual({ type: "standby_ready" });
  await inspect(f.network, "restore");
  const connectPath = `/networks/${f.network}/connect/${leaf.device}`;
  const source = await authenticated(connectPath, (nonce) =>
    f.manager.proof(connectPath, nonce, true),
  );
  expect((await source.json()).type).toBe("connected");
  expect((await standby.json()).type).toBe("connected");
  const body = new Uint8Array([1, 2, 3, 4]).buffer;
  source.send(body);
  expect(new Uint8Array((await standby.next()) as ArrayBuffer)).toEqual(
    new Uint8Array(body),
  );
  source.close();
  standby.close();
  target.close();
});
