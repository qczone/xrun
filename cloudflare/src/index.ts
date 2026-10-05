import { DurableObject } from "cloudflare:workers";
import { DEVICE, object, randomRoute, verifyProof } from "./auth";

export interface Env {
  NETWORKS: DurableObjectNamespace<XrunRelay>;
  RELAY_ROUTE: string;
  XRUN_VERSION: string;
}
const FRAME = 64 * 1024;
const WINDOW = 16 * FRAME;
const IDLE = 300_000;
interface Attachment {
  id: string;
  role: "auth" | "verifying" | "control" | "pending" | "source" | "target" | "closed";
  network: string;
  ip: string;
  action?: "status" | "control" | "connect";
  path?: string;
  nonce?: string;
  device?: string;
  manager?: boolean;
  target?: string;
  generation?: string;
  sid?: string;
  anonymous?: boolean;
  peer?: string;
  outstanding?: number;
  deadline?: number;
}
interface Route { network: string; action: "status" | "control" | "connect" | "attach"; target?: string; generation?: string; sid?: string; path: string }
function route(path: string, prefix: string): Route | undefined {
  if (!/^[a-z2-7]{26}$/.test(prefix) || !path.startsWith(`/${prefix}/`)) return;
  const relative = path.slice(prefix.length + 1);
  const parts = relative.split("/").slice(1);
  if (parts[0] !== "networks" || !/^(net_[a-z2-7]{52}|probe)$/.test(parts[1])) return;
  const [_, network, action, target, generation, sid] = parts;
  if ((action === "status" || action === "control") && parts.length === 3) return { network, action, path: relative };
  if (action === "connect" && parts.length === 4 && DEVICE.test(target)) return { network, action, target, path: relative };
  if (action === "attach" && parts.length === 6 && DEVICE.test(target) && /^[a-f0-9]{32}$/.test(generation) && /^[a-f0-9]{32}$/.test(sid)) return { network, action, target, generation, sid, path: relative };
}
function error(code: string, message: string, status = 400): Response {
  return Response.json({ type: "error", code, message }, { status });
}
export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const parsed = route(url.pathname, env.RELAY_ROUTE);
    if (request.method !== "GET" || url.search || !parsed) return new Response("Not found", { status: 404 });
    if (request.headers.get("X-Xrun-Version") !== env.XRUN_VERSION) return error("VERSION_MISMATCH", "Relay release differs", 409);
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") return error("INVALID_REQUEST", "WebSocket required", 426);
    const headers = new Headers(request.headers);
    headers.set("X-Xrun-Peer", request.headers.get("CF-Connecting-IP") || "unknown");
    return env.NETWORKS.getByName(parsed.network).fetch(new Request(request, { headers }));
  },
} satisfies ExportedHandler<Env>;

/** Only live routing bindings and ciphertext windows survive with sockets. */
export class XrunRelay extends DurableObject<Env> {
  private sockets(): WebSocket[] {
    return this.ctx.getWebSockets().filter((ws) => ws.readyState === WebSocket.OPEN && this.state(ws).role !== "closed");
  }
  private state(ws: WebSocket): Attachment { return ws.deserializeAttachment() as Attachment; }
  private save(ws: WebSocket, state: Attachment): void { ws.serializeAttachment(state); }
  private find(id: string): WebSocket | undefined { return this.sockets().find((ws) => this.state(ws).id === id); }
  private control(device: string): WebSocket | undefined {
    return this.sockets().find((ws) => { const a = this.state(ws); return a.role === "control" && a.device === device; });
  }
  private send(ws: WebSocket, value: object): void { ws.send(JSON.stringify(value)); }
  private close(ws: WebSocket, code = 1000, reason = "Connection closed"): void {
    const a = this.state(ws);
    if (a.role === "closed") return;
    a.role = "closed";
    this.save(ws, a);
    try { ws.close(code, reason); } catch { /* Socket may already be closed. */ }
    if (a.peer) { const peer = this.find(a.peer); if (peer) this.close(peer, code, reason); }
    if (a.device && a.generation) {
      for (const tunnel of this.sockets()) {
        const b = this.state(tunnel);
        if (b.target === a.device && b.generation === a.generation) this.close(tunnel, 1000, "Control disconnected");
      }
    }
  }
  private reject(ws: WebSocket, code: string, message: string): void {
    try { this.send(ws, { type: "error", code, message }); } finally { this.close(ws, 1008, code); }
  }
  private async schedule(): Promise<void> {
    const deadlines = this.sockets().map((ws) => this.state(ws).deadline).filter((n): n is number => n !== undefined);
    if (!deadlines.length) { await this.ctx.storage.deleteAlarm(); return; }
    const next = Math.min(...deadlines);
    const current = await this.ctx.storage.getAlarm();
    if (current === null || current > next) await this.ctx.storage.setAlarm(Math.max(Date.now() + 1, next));
  }
  async alarm(): Promise<void> {
    const now = Date.now();
    for (const ws of this.sockets()) {
      const a = this.state(ws);
      if (a.deadline !== undefined && a.deadline <= now) {
        const message = a.role === "auth" || a.role === "verifying" ? "Member authentication timed out"
          : a.role === "pending" ? "Target did not connect in time" : "Relay session was idle too long";
        this.reject(ws, "CONNECT_TIMEOUT", message);
      }
    }
    await this.schedule();
  }
  async fetch(request: Request): Promise<Response> {
    const parsed = route(new URL(request.url).pathname, this.env.RELAY_ROUTE);
    if (!parsed) return new Response("Not found", { status: 404 });
    const sockets = this.sockets();
    if (this.ctx.getWebSockets().length >= 512) return error("CONNECTION_LIMIT", "Too many connections", 429);
    const ip = request.headers.get("X-Xrun-Peer") || "unknown";
    if (parsed.action !== "attach" && sockets.filter((ws) => {
      const a = this.state(ws); return (a.role === "auth" || a.role === "verifying") && a.ip === ip;
    }).length >= 8) return error("CONNECTION_LIMIT", "Too many unverified connections", 429);
    let pending: WebSocket | undefined;
    if (parsed.action === "attach") {
      const control = this.control(parsed.target!);
      if (!control || this.state(control).generation !== parsed.generation) return error("INVALID_SESSION", "Control binding mismatch");
      pending = sockets.find((ws) => { const a = this.state(ws); return a.role === "pending" && a.sid === parsed.sid && a.target === parsed.target && a.generation === parsed.generation && a.deadline! > Date.now(); });
      if (!pending) return error("INVALID_SESSION", "Session missing, expired or already claimed");
    }
    const [client, server] = Object.values(new WebSocketPair());
    const state: Attachment = { id: crypto.randomUUID(), role: "auth", network: parsed.network, ip, action: parsed.action === "attach" ? undefined : parsed.action, target: parsed.target, path: parsed.path, nonce: randomRoute(), deadline: Date.now() + 5000 };
    this.ctx.acceptWebSocket(server);
    this.save(server, state);
    if (pending) {
      const source = this.state(pending);
      source.role = "source"; source.peer = state.id; source.outstanding = 0; source.deadline = Date.now() + IDLE;
      this.save(pending, source);
      this.save(server, { id: state.id, role: "target", network: source.network, ip, peer: source.id, target: source.target, generation: source.generation, sid: source.sid, outstanding: 0, deadline: Date.now() + IDLE });
      this.send(server, { type: "connected", flow_control: true });
      this.send(pending, { type: "connected", flow_control: true });
    } else {
      this.send(server, { type: "challenge", nonce: state.nonce });
    }
    await this.schedule();
    return new Response(null, { status: 101, webSocket: client });
  }
  private async authenticate(ws: WebSocket, state: Attachment, message: string): Promise<void> {
    if (message.length > 24 * 1024 || state.deadline! <= Date.now()) throw new Error("Invalid proof size or timeout");
    const value: unknown = JSON.parse(message);
    if (!object(value, ["type", "proof"]) || value.type !== "authenticate") throw new Error("Expected member proof");
    state.role = "verifying";
    this.save(ws, state);
    const proof = value.proof == null ? undefined : await verifyProof(value.proof, state.network, state.path!, state.nonce!, this.env.XRUN_VERSION);
    if (ws.readyState !== WebSocket.OPEN || this.state(ws).role !== "verifying") return;
    if (state.deadline! <= Date.now()) throw new Error("Authentication timed out");
    if (state.action === "status") {
      if (!proof) throw new Error("Member proof required");
      this.send(ws, { type: "status", devices: this.sockets().filter((socket) => this.state(socket).role === "control").map((socket) => this.state(socket).device).sort() });
      this.close(ws);
    } else if (state.action === "control") {
      if (!proof) throw new Error("Member proof required");
      const previous = this.control(proof.device);
      if (!previous && this.sockets().filter((socket) => this.state(socket).role === "control").length >= 256) {
        this.reject(ws, "CONNECTION_LIMIT", "Too many control connections"); return;
      }
      if (previous) this.close(previous, 1000, "Control replaced");
      const generation = crypto.randomUUID().replaceAll("-", "");
      this.save(ws, { id: state.id, role: "control", network: state.network, ip: state.ip, device: proof.device, manager: proof.manager, generation });
      this.send(ws, { type: "hello_ack", generation });
    } else {
      const target = this.control(state.target!);
      if (!target) { this.reject(ws, "DEVICE_OFFLINE", "Target is offline"); return; }
      if (!proof && !this.state(target).manager) throw new Error("Member proof required");
      const sources = this.sockets().map((socket) => this.state(socket)).filter((a) => a.role === "source" || a.role === "pending");
      const toTarget = sources.filter((a) => a.target === state.target);
      if (sources.length >= 32 || toTarget.length >= 32 || sources.filter((a) => a.ip === state.ip).length >= 16 || (!proof && toTarget.filter((a) => a.anonymous).length >= 4)) {
        this.reject(ws, "SESSION_LIMIT", "Too many concurrent relay sessions"); return;
      }
      const sid = crypto.randomUUID().replaceAll("-", "");
      this.save(ws, { id: state.id, role: "pending", network: state.network, ip: state.ip, target: state.target, generation: this.state(target).generation, sid, anonymous: !proof, deadline: Date.now() + 10_000 });
      this.send(target, { type: "incoming", session_id: sid });
    }
  }
  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const state = this.state(ws);
    try {
      if (state.role === "auth") {
        if (typeof message !== "string") throw new Error("Expected member proof");
        await this.authenticate(ws, state, message);
      } else if (state.role === "control") {
        if (typeof message !== "string" || message.length > 4096) throw new Error("Invalid control message");
        const value: unknown = JSON.parse(message);
        if (!object(value, ["type", "session_id", "error"]) || value.type !== "reject" || typeof value.session_id !== "string" || !object(value.error, ["type", "code", "message"]) || value.error.type !== "error" || typeof value.error.code !== "string" || typeof value.error.message !== "string") throw new Error("Invalid rejection");
        const pending = this.sockets().find((socket) => { const a = this.state(socket); return a.role === "pending" && a.target === state.device && a.generation === state.generation && a.sid === value.session_id; });
        if (pending) this.reject(pending, value.error.code, value.error.message);
      } else if (state.role === "source" || state.role === "target") {
        const peer = this.find(state.peer!);
        if (!peer) { this.close(ws); return; }
        if (typeof message === "string") {
          if (message.length > 128) throw new Error("Invalid acknowledgement");
          const value: unknown = JSON.parse(message);
          const sender = this.state(peer);
          if (!object(value, ["type", "bytes"]) || value.type !== "ack" || typeof value.bytes !== "number" || !Number.isSafeInteger(value.bytes) || value.bytes <= 0 || value.bytes > sender.outstanding!) throw new Error("Invalid acknowledgement");
          sender.outstanding! -= value.bytes;
          this.save(peer, sender);
          peer.send(message);
        } else {
          if (!message.byteLength || message.byteLength > FRAME || state.outstanding! + message.byteLength > WINDOW) {
            this.reject(ws, "MESSAGE_TOO_LARGE", "Ciphertext window exceeded"); return;
          }
          state.outstanding! += message.byteLength;
          peer.send(message);
        }
        state.deadline = Date.now() + IDLE;
        this.save(ws, state);
      } else {
        throw new Error("Unexpected relay message");
      }
    } catch {
      this.reject(ws, state.role === "auth" || state.role === "verifying" ? "UNAUTHENTICATED" : "INVALID_MESSAGE", "Relay message rejected");
    }
    await this.schedule();
  }
  async webSocketClose(ws: WebSocket): Promise<void> { this.close(ws); await this.schedule(); }
  async webSocketError(ws: WebSocket): Promise<void> { this.close(ws, 1011, "Transport error"); await this.schedule(); }
}
