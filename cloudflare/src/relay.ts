import { DurableObject } from "cloudflare:workers";
import { object, randomRoute, verifyProof } from "./auth";
import { Connections, type Connection } from "./connections";
import {
  ACK_MESSAGE_BYTES,
  ANONYMOUS_PER_TARGET,
  AUTH_TIMEOUT_MS,
  AUTHENTICATING_PER_IP,
  CONNECT_TIMEOUT_MS,
  CONNECTION_LIMIT,
  CONTROL_LIMIT,
  CONTROL_MESSAGE_BYTES,
  FRAME,
  IDLE,
  PROOF_MESSAGE_BYTES,
  SESSIONS,
  WINDOW,
} from "./limits";
import { route, type AttachRoute } from "./routes";
import {
  base,
  expired,
  session,
  tunnel,
  type ChallengeState,
  type ControlState,
  type PendingState,
  type TunnelState,
} from "./state";

export interface Env {
  NETWORKS: DurableObjectNamespace<XrunRelay>;
  RELAY_ROUTE: string;
  XRUN_PROTOCOL_MIN: number;
  XRUN_PROTOCOL_MAX: number;
}
export function error(code: string, message: string, status = 400): Response {
  return Response.json({ type: "error", code, message }, { status });
}

/** Live routing bindings and ciphertext windows survive with hibernating sockets. */
export class XrunRelay extends DurableObject<Env> {
  private connections: Connections;
  private nextAlarm: number | null = null;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.connections = new Connections(ctx.getWebSockets());
    this.validateRestoredBindings();
    ctx.blockConcurrencyWhile(async () => {
      this.nextAlarm = await ctx.storage.getAlarm();
      await this.schedule();
    });
  }
  /** The same restoration path is used after eviction and by workerd recovery tests. */
  protected restoreSockets(): void {
    this.connections = new Connections(this.ctx.getWebSockets());
    this.validateRestoredBindings();
  }
  private validateRestoredBindings(): void {
    for (const { socket, state } of [...this.connections.all()]) {
      if (!session(state)) continue;
      const control = this.connections.control(state.target);
      if (!control || control.state.generation !== state.generation) {
        this.close(socket, 1008, "Restored control binding missing");
        continue;
      }
      if (!tunnel(state)) continue;
      const peer = this.connections.find(state.peer)?.state;
      const bound =
        peer &&
        tunnel(peer) &&
        peer.peer === state.id &&
        peer.role !== state.role &&
        peer.network === state.network &&
        peer.target === state.target &&
        peer.generation === state.generation &&
        peer.sid === state.sid;
      if (!bound) this.close(socket, 1008, "Restored tunnel binding missing");
    }
  }
  private send(ws: WebSocket, value: object): void {
    ws.send(JSON.stringify(value));
  }
  private close(
    ws: WebSocket,
    code = 1000,
    reason = "Connection closed",
  ): void {
    const state = this.connections.get(ws)?.state;
    if (!state) return;
    // Capture bindings before discarding all role-specific fields.
    const peer = tunnel(state)
      ? this.connections.find(state.peer)?.socket
      : undefined;
    const bound =
      state.role === "control"
        ? this.connections.bound(state.device, state.generation)
        : [];
    this.connections.save(ws, { ...base(state), role: "closed" });
    try {
      ws.close(code, reason);
    } catch {
      /* Socket may already be closed. */
    }
    if (peer) this.close(peer, code, reason);
    for (const socket of bound)
      this.close(socket, 1000, "Control disconnected");
  }
  private reject(ws: WebSocket, code: string, message: string): void {
    try {
      this.send(ws, { type: "error", code, message });
    } finally {
      this.close(ws, 1008, code);
    }
  }
  private async schedule(): Promise<void> {
    let next: number | null = null;
    for (const { state } of this.connections.all()) {
      if ("deadline" in state && (next === null || state.deadline < next))
        next = state.deadline;
    }
    if (next === null) {
      if (this.nextAlarm !== null) {
        this.nextAlarm = null;
        await this.ctx.storage.deleteAlarm();
      }
    } else if (this.nextAlarm === null || this.nextAlarm > next) {
      this.nextAlarm = Math.max(Date.now() + 1, next);
      await this.ctx.storage.setAlarm(this.nextAlarm);
    }
  }
  async alarm(): Promise<void> {
    this.nextAlarm = null;
    const now = Date.now();
    for (const { socket, state } of [...this.connections.all()]) {
      if (!this.connections.get(socket) || !expired(state, now)) continue;
      const message =
        state.role === "auth" || state.role === "verifying"
          ? "Member authentication timed out"
          : state.role === "pending"
            ? "Target did not connect in time"
            : "Relay session was idle too long";
      this.reject(socket, "CONNECT_TIMEOUT", message);
    }
    await this.schedule();
  }
  private pending(parsed: AttachRoute): Connection<PendingState> | undefined {
    const control = this.connections.control(parsed.target);
    if (!control || control.state.generation !== parsed.generation) return;
    return this.connections.claim(
      parsed.target,
      parsed.generation,
      parsed.sid,
      Date.now(),
    );
  }
  async fetch(request: Request): Promise<Response> {
    const parsed = route(new URL(request.url).pathname, this.env.RELAY_ROUTE);
    if (!parsed) return new Response("Not found", { status: 404 });
    if (this.connections.size >= CONNECTION_LIMIT)
      return error("CONNECTION_LIMIT", "Too many connections", 429);
    const ip = request.headers.get("X-Xrun-Peer") || "unknown";
    if (
      parsed.action !== "attach" &&
      this.connections.authenticating(ip) >= AUTHENTICATING_PER_IP
    ) {
      return error("CONNECTION_LIMIT", "Too many unverified connections", 429);
    }
    const pending =
      parsed.action === "attach" ? this.pending(parsed) : undefined;
    if (parsed.action === "attach" && !pending)
      return error(
        "INVALID_SESSION",
        "Session missing, expired or already claimed",
      );
    const [client, server] = Object.values(new WebSocketPair());
    const connection = { id: crypto.randomUUID(), network: parsed.network, ip };
    this.ctx.acceptWebSocket(server);
    if (pending) {
      const source = pending.state;
      const binding = {
        target: source.target,
        generation: source.generation,
        sid: source.sid,
      };
      const deadline = Date.now() + IDLE;
      this.connections.save(pending.socket, {
        ...source,
        role: "source",
        peer: connection.id,
        outstanding: 0,
        deadline,
      });
      this.connections.save(server, {
        ...connection,
        ...binding,
        role: "target",
        peer: source.id,
        outstanding: 0,
        deadline,
      });
      this.send(server, { type: "connected", flow_control: true });
      this.send(pending.socket, { type: "connected", flow_control: true });
    } else if (parsed.action !== "attach") {
      const nonce = randomRoute();
      this.connections.save(server, {
        ...connection,
        ...parsed,
        role: "auth",
        nonce,
        deadline: Date.now() + AUTH_TIMEOUT_MS,
      });
      this.send(server, { type: "challenge", nonce });
    }
    await this.schedule();
    return new Response(null, {
      status: 101,
      webSocket: client,
      headers: {
        "X-Xrun-Protocol":
          request.headers.get("X-Xrun-Negotiated-Protocol") ?? "",
      },
    });
  }
  protected async proof(
    value: unknown,
    state: ChallengeState,
  ): Promise<{ device: string; manager: boolean } | undefined> {
    if (value == null) return;
    return verifyProof(value, state.network, state.path, state.nonce);
  }
  private async authenticate(
    ws: WebSocket,
    state: ChallengeState,
    message: string,
  ): Promise<void> {
    if (message.length > PROOF_MESSAGE_BYTES || expired(state, Date.now()))
      throw new Error("Invalid proof size or timeout");
    const value: unknown = JSON.parse(message);
    if (!object(value) || value.type !== "authenticate")
      throw new Error("Expected member proof");
    this.connections.save(ws, { ...state, role: "verifying" });
    const proof = await this.proof(value.proof, state);
    const current = this.connections.get(ws)?.state;
    const sameChallenge =
      current?.role === "verifying" &&
      current.id === state.id &&
      current.nonce === state.nonce;
    if (ws.readyState !== WebSocket.OPEN || !sameChallenge) return;
    if (expired(current, Date.now()))
      throw new Error("Authentication timed out");
    if (state.action === "status") {
      if (!proof) throw new Error("Member proof required");
      this.send(ws, { type: "status", devices: this.connections.devices() });
      this.close(ws);
    } else if (state.action === "control") {
      if (!proof) throw new Error("Member proof required");
      this.registerControl(ws, state, proof);
    } else {
      this.startSession(ws, state, proof !== undefined);
    }
  }
  private registerControl(
    ws: WebSocket,
    state: ChallengeState,
    proof: { device: string; manager: boolean },
  ): void {
    const previous = this.connections.control(proof.device);
    if (!previous && this.connections.controlCount >= CONTROL_LIMIT) {
      this.reject(ws, "CONNECTION_LIMIT", "Too many control connections");
      return;
    }
    if (previous) this.close(previous.socket, 1000, "Control replaced");
    const generation = crypto.randomUUID().replaceAll("-", "");
    this.connections.save(ws, {
      ...base(state),
      role: "control",
      device: proof.device,
      manager: proof.manager,
      generation,
    });
    this.send(ws, { type: "hello_ack", generation });
  }
  private startSession(
    ws: WebSocket,
    state: ChallengeState & { action: "connect"; target: string },
    member: boolean,
  ): void {
    const target = this.connections.control(state.target);
    if (!target) {
      this.reject(ws, "DEVICE_OFFLINE", "Target is offline");
      return;
    }
    if (!member && !target.state.manager)
      throw new Error("Member proof required");
    const anonymousFull =
      !member &&
      this.connections.anonymous(state.target) >= ANONYMOUS_PER_TARGET;
    if (this.connections.sourceCount >= SESSIONS || anonymousFull) {
      this.reject(ws, "SESSION_LIMIT", "Too many concurrent relay sessions");
      return;
    }
    const sid = crypto.randomUUID().replaceAll("-", "");
    this.connections.save(ws, {
      ...base(state),
      role: "pending",
      target: state.target,
      generation: target.state.generation,
      sid,
      anonymous: !member,
      deadline: Date.now() + CONNECT_TIMEOUT_MS,
    });
    this.send(target.socket, { type: "incoming", session_id: sid });
  }
  private controlMessage(
    ws: WebSocket,
    state: ControlState,
    message: string | ArrayBuffer,
  ): void {
    if (typeof message !== "string" || message.length > CONTROL_MESSAGE_BYTES)
      throw new Error("Invalid control message");
    const value: unknown = JSON.parse(message);
    if (
      !object(value) ||
      value.type !== "reject" ||
      typeof value.session_id !== "string"
    ) {
      throw new Error("Invalid rejection");
    }
    if (
      !object(value.error) ||
      value.error.type !== "error" ||
      typeof value.error.code !== "string" ||
      typeof value.error.message !== "string"
    )
      throw new Error("Invalid rejection error");
    const pending = this.connections.claim(
      state.device,
      state.generation,
      value.session_id,
      Date.now(),
    );
    if (pending)
      this.reject(pending.socket, value.error.code, value.error.message);
  }
  private tunnelMessage(
    ws: WebSocket,
    state: TunnelState,
    message: string | ArrayBuffer,
  ): void {
    const peer = this.connections.find(state.peer);
    if (!peer || !tunnel(peer.state)) {
      this.close(ws);
      return;
    }
    if (expired(state, Date.now())) {
      this.reject(ws, "CONNECT_TIMEOUT", "Relay session was idle too long");
      return;
    }
    if (typeof message === "string") {
      if (message.length > ACK_MESSAGE_BYTES)
        throw new Error("Invalid acknowledgement");
      const value: unknown = JSON.parse(message);
      if (
        !object(value) ||
        value.type !== "ack" ||
        typeof value.bytes !== "number" ||
        !Number.isSafeInteger(value.bytes) ||
        value.bytes <= 0 ||
        value.bytes > peer.state.outstanding
      ) {
        throw new Error("Invalid acknowledgement");
      }
      this.connections.save(peer.socket, {
        ...peer.state,
        outstanding: peer.state.outstanding - value.bytes,
      });
    } else {
      if (
        !message.byteLength ||
        message.byteLength > FRAME ||
        state.outstanding + message.byteLength > WINDOW
      ) {
        this.reject(ws, "MESSAGE_TOO_LARGE", "Ciphertext window exceeded");
        return;
      }
      state = { ...state, outstanding: state.outstanding + message.byteLength };
    }
    // Budget changes must always survive eviction. Persist the exact idle expiry
    // in the same write, so wake-up cannot expire a recently active peer early.
    this.connections.save(ws, { ...state, deadline: Date.now() + IDLE });
    peer.socket.send(message);
  }
  async webSocketMessage(
    ws: WebSocket,
    message: string | ArrayBuffer,
  ): Promise<void> {
    const state = this.connections.get(ws)?.state;
    if (!state) return;
    try {
      if (state.role === "auth") {
        if (typeof message !== "string")
          throw new Error("Expected member proof");
        await this.authenticate(ws, state, message);
      } else if (state.role === "control") {
        this.controlMessage(ws, state, message);
      } else if (tunnel(state)) {
        this.tunnelMessage(ws, state, message);
        // Extending an idle expiry cannot bring the next alarm forward. No
        // connection enumeration or storage read is needed for ordinary frames.
        return;
      } else {
        throw new Error("Unexpected relay message");
      }
    } catch {
      if (!this.connections.get(ws)) return;
      const authenticating =
        state.role === "auth" || state.role === "verifying";
      this.reject(
        ws,
        authenticating ? "UNAUTHENTICATED" : "INVALID_MESSAGE",
        "Relay message rejected",
      );
    }
    await this.schedule();
  }
  async webSocketClose(ws: WebSocket): Promise<void> {
    this.close(ws);
    await this.schedule();
  }
  async webSocketError(ws: WebSocket): Promise<void> {
    this.close(ws, 1011, "Transport error");
    await this.schedule();
  }
}
