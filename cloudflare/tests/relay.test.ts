import { afterAll, beforeAll, expect, test } from "bun:test";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { randomRoute } from "../src/auth";
import { fixture, VERSION } from "./fixtures";
const secret = randomRoute();
let runtime: Miniflare;
let origin: string;
beforeAll(async () => {
  runtime = new Miniflare(convertV4MiniflareOptions({
    modules: true, scriptPath: `${import.meta.dir}/../.wrangler/build/index.js`,
    compatibilityDate: "2026-10-03",
    durableObjects: { NETWORKS: { className: "XrunRelay", useSQLite: true } },
    bindings: { RELAY_ROUTE: secret, XRUN_VERSION: VERSION },
  }));
  origin = (await runtime.ready).origin;
});
afterAll(async () => { await runtime?.dispose(); });
class Socket {
  private queue: (string | ArrayBuffer)[] = [];
  private waiting?: { resolve: (value: string | ArrayBuffer) => void; reject: (error: Error) => void };
  private closed = false;
  constructor(readonly ws: NonNullable<Awaited<ReturnType<Miniflare["dispatchFetch"]>>["webSocket"]>) {
    ws.addEventListener("message", (event) => {
      const value = event.data as string | ArrayBuffer;
      if (this.waiting) { const { resolve } = this.waiting; this.waiting = undefined; resolve(value); } else this.queue.push(value);
    });
    ws.addEventListener("close", () => { this.closed = true; this.waiting?.reject(new Error("closed")); this.waiting = undefined; });
    ws.accept();
  }
  send(value: object | ArrayBuffer) { this.ws.send(value instanceof ArrayBuffer ? value : JSON.stringify(value)); }
  async next(): Promise<string | ArrayBuffer> {
    if (this.queue.length) return this.queue.shift()!;
    if (this.closed) throw new Error("closed");
    return Promise.race([
      new Promise<string | ArrayBuffer>((resolve, reject) => { this.waiting = { resolve, reject }; }),
      new Promise<never>((_, reject) => { const timer = setTimeout(() => reject(new Error("timeout")), 5000); timer.unref(); }),
    ]);
  }
  async json() { return JSON.parse(await this.next() as string); }
  close() { this.ws.close(); }
}
async function open(path: string): Promise<Socket> {
  const response = await runtime.dispatchFetch(`${origin}/${secret}${path}`, { headers: { Upgrade: "websocket", "X-Xrun-Version": VERSION } });
  if (!response.webSocket) throw new Error(`Upgrade ${response.status}: ${await response.text()}`);
  return new Socket(response.webSocket);
}
async function authenticated(path: string, proof: (nonce: string) => Promise<unknown>) {
  const socket = await open(path);
  const challenge = await socket.json();
  expect(challenge.type).toBe("challenge");
  socket.send({ type: "authenticate", proof: await proof(challenge.nonce) });
  return socket;
}

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
    headers: { Upgrade: "websocket", "X-Xrun-Version": VERSION, "X-Xrun-Peer": "forged" },
  });
  expect(response.status).toBe(429);
  expect(response.webSocket).toBeNull();
  for (const socket of waiting) socket.close();
});

// Runs the deployed module in workerd rather than replacing Durable Object APIs.
test("secret route, component version and proof protect existing device routes", async () => {
  expect((await runtime.dispatchFetch(`${origin}/wrong/networks/probe/status`)).status).toBe(404);
  expect((await runtime.dispatchFetch(`${origin}/${secret}/networks/probe/status`, { headers: { Upgrade: "websocket", "X-Xrun-Version": "wrong" } })).status).toBe(409);
  const f = await fixture();
  const target = await f.member(), source = await f.member();
  const path = `/networks/${f.network}/control`;
  const control = await authenticated(path, (nonce) => target.proof(path, nonce));
  const hello = await control.json();
  expect(hello.type).toBe("hello_ack");
  for (const make of [
    async (nonce: string) => ({ ...await source.proof(path, nonce), device_id: target.device }),
    async (nonce: string) => ({ ...await target.proof(path, nonce), signature: (await source.proof(path, nonce)).signature }),
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
  const sender = await authenticated(connect, (nonce) => source.proof(connect, nonce));
  const incoming = await control.json();
  expect(incoming.type).toBe("incoming");
  const attach = `/networks/${f.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`;
  const receiver = await open(attach);
  expect(await receiver.json()).toEqual({ type: "connected", flow_control: true });
  expect(await sender.json()).toEqual({ type: "connected", flow_control: true });
  await expect(open(attach)).rejects.toThrow("Upgrade 400");
  // Acknowledgements release the window without limiting cumulative transfer.
  for (let i = 0; i < 140; i++) {
    const bytes = new Uint8Array(64 * 1024).fill(i % 256).buffer;
    sender.send(bytes);
    expect(new Uint8Array(await receiver.next() as ArrayBuffer)).toEqual(new Uint8Array(bytes));
    receiver.send({ type: "ack", bytes: bytes.byteLength });
    expect(await sender.json()).toEqual({ type: "ack", bytes: bytes.byteLength });
  }
  const replacement = await authenticated(path, (nonce) => target.proof(path, nonce));
  expect((await replacement.json()).generation).not.toBe(hello.generation);
  await expect(sender.next()).rejects.toThrow("closed");
  await expect(open(attach)).rejects.toThrow("Upgrade 400");
  for (const socket of [control, anonymous, sender, receiver, replacement]) socket.close();
}, 20_000);

test("anonymous pairing is manager-only, limited, and cleaned on control disconnect", async () => {
  const f = await fixture(), manager = await f.member();
  const path = `/networks/${f.network}/control`;
  const control = await authenticated(path, (nonce) => manager.proof(path, nonce, true));
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
  for (const socket of pending) { await expect(socket.next()).rejects.toThrow("closed"); socket.close(); }
  overflow.close();
});

test("unacknowledged ciphertext is bounded and forged credit closes both peers", async () => {
  for (const forged of [false, true]) {
    const f = await fixture(), target = await f.member(), source = await f.member();
    const path = `/networks/${f.network}/control`;
    const control = await authenticated(path, (nonce) => target.proof(path, nonce));
    const hello = await control.json();
    const connect = `/networks/${f.network}/connect/${target.device}`;
    const sender = await authenticated(connect, (nonce) => source.proof(connect, nonce));
    const incoming = await control.json();
    const receiver = await open(`/networks/${f.network}/attach/${target.device}/${hello.generation}/${incoming.session_id}`);
    await receiver.json(); await sender.json();
    if (forged) {
      receiver.send({ type: "ack", bytes: 1 });
      expect((await receiver.json()).code).toBe("INVALID_MESSAGE");
      await expect(sender.next()).rejects.toThrow("closed");
    } else {
      for (let i = 0; i < 64; i++) { sender.send(new Uint8Array(64 * 1024).buffer); await receiver.next(); }
      sender.send(new Uint8Array(1).buffer);
      expect((await sender.json()).code).toBe("MESSAGE_TOO_LARGE");
      await expect(receiver.next()).rejects.toThrow("closed");
    }
    control.close(); sender.close(); receiver.close();
  }
});

test("pending and active sessions share the network buffer budget and release their slots", async () => {
  const f = await fixture(), source = await f.member();
  const targets = await Promise.all([f.member(), f.member()]);
  const controlPath = `/networks/${f.network}/control`;
  const controls: Socket[] = [], generations: string[] = [], sources: Socket[] = [], receivers: Socket[] = [];
  for (const target of targets) {
    const control = await authenticated(controlPath, (nonce) => target.proof(controlPath, nonce));
    generations.push((await control.json()).generation);
    controls.push(control);
  }
  for (let i = 0; i < 8; i++) {
    const index = i % 2, target = targets[index];
    const connect = `/networks/${f.network}/connect/${target.device}`;
    const sender = await authenticated(connect, (nonce) => source.proof(connect, nonce));
    sources.push(sender);
    const incoming = await controls[index].json();
    expect(incoming.type).toBe("incoming");
    if (i < 4) {
      const receiver = await open(`/networks/${f.network}/attach/${target.device}/${generations[index]}/${incoming.session_id}`);
      expect((await receiver.json()).type).toBe("connected");
      expect((await sender.json()).type).toBe("connected");
      receivers.push(receiver);
    }
  }
  const connect = `/networks/${f.network}/connect/${targets[1].device}`;
  const overflow = await authenticated(connect, (nonce) => source.proof(connect, nonce));
  expect((await overflow.json()).code).toBe("SESSION_LIMIT");
  sources[0].close();
  await expect(receivers[0].next()).rejects.toThrow("closed");
  const replacement = await authenticated(connect, (nonce) => source.proof(connect, nonce));
  expect((await controls[1].json()).type).toBe("incoming");
  for (const socket of [...controls, ...sources.slice(1), ...receivers, overflow, replacement]) socket.close();
});
