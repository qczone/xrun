/** One hibernating object forwards one admitted encrypted channel. Frames never visit the coordinator. */
import { DurableObject } from "cloudflare:workers";
import { object } from "./auth";
import {
  verifyAuthority,
  requireFresh,
  successor,
  type AuthorityState,
} from "./authority";
import { FRAME, WINDOW, ACK_MESSAGE_BYTES, IDLE } from "./limits";
import { RELAY_OBJECT } from "./routes";
import type { Env } from "./relay";
import { legs, type ShardSlot } from "./shards";
import {
  trafficCounters,
  TRAFFIC_DAY,
  type TrafficLeg,
  type TrafficCounter,
} from "./traffic-state";
interface Leg {
  side: "source" | "target";
  id: string;
  outstanding: number;
  pairingBytes: number;
  traffic: TrafficCounter[];
  deadline: number;
}
interface Saved {
  slot: ShardSlot;
  final: TrafficLeg[] | null;
  incomplete: boolean;
}
export class XrunData extends DurableObject<Env> {
  private slot: ShardSlot | null = null;
  private final: TrafficLeg[] | null = null;
  private incomplete = false;
  private flushAt: number | null = null;
  private alarmAt: number | null = null;
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    ctx.storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS data_slot(id INTEGER PRIMARY KEY CHECK(id=1),data TEXT NOT NULL) STRICT",
    );
    const row = ctx.storage.sql
      .exec<{ data: string }>("SELECT data FROM data_slot WHERE id=1")
      .toArray()[0];
    if (row) {
      const saved = JSON.parse(row.data) as Saved;
      this.slot = saved.slot;
      this.final = saved.final;
      this.incomplete = saved.incomplete;
    }
    ctx.blockConcurrencyWhile(async () => {
      await this.restore();
    });
  }
  /** Also exercised by the owned workerd eviction fixture. */
  protected async restore(): Promise<void> {
    if (!this.slot) return;
    await verifyAuthority(this.slot.authority, this.slot.network);
    this.alarmAt = await this.ctx.storage.getAlarm();
    let invalid = false;
    const seen = new Set<string>();
    for (const socket of this.ctx.getWebSockets()) {
      const leg = this.leg(socket);
      if (!leg || seen.has(leg.side)) {
        invalid = true;
        try {
          socket.close(1008, "Invalid restored data binding");
        } catch {
          /* Already closed. */
        }
      } else seen.add(leg.side);
    }
    if (
      invalid ||
      (!this.slot.warm &&
        !this.slot.sourceTicket &&
        !this.slot.targetTicket &&
        seen.size !== 2)
    ) {
      this.incomplete = true;
      await this.shutdown("Restored peer missing");
    } else await this.schedule();
  }
  private store(): void {
    if (!this.slot) return;
    const saved: Saved = {
      slot: this.slot,
      final: this.final,
      incomplete: this.incomplete,
    };
    this.ctx.storage.sql.exec(
      "INSERT INTO data_slot VALUES(1,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
      JSON.stringify(saved),
    );
  }
  /** Internal RPC only; the public Worker exposes no configuration endpoints. */
  async prepare(slot: ShardSlot): Promise<void> {
    await verifyAuthority(slot.authority, slot.network);
    requireFresh(slot.authority);
    if (this.slot) throw new Error("Data reservation already prepared");
    this.slot = slot;
    this.store();
    await this.schedule();
  }
  async assign(slot: ShardSlot): Promise<void> {
    if (
      !this.slot ||
      this.slot.closed ||
      !this.slot.warm ||
      this.slot.sid !== slot.sid ||
      this.slot.network !== slot.network ||
      this.slot.target !== slot.target ||
      this.slot.generation !== slot.generation
    )
      throw new Error("Warm binding mismatch");
    await verifyAuthority(slot.authority, slot.network);
    successor(this.slot.authority, slot.authority);
    requireFresh(slot.authority);
    slot.targetTicket = this.slot.targetTicket;
    this.slot = slot;
    this.store();
    await this.schedule();
  }
  async authority(state: AuthorityState): Promise<void> {
    if (!this.slot || this.slot.closed) return;
    await verifyAuthority(state, this.slot.network);
    successor(this.slot.authority, state);
    if (
      state.roster.roster.version ===
        this.slot.authority.roster.roster.version &&
      state.lease.lease.issued_at_ms <
        this.slot.authority.lease.lease.issued_at_ms
    )
      return;
    const generation = this.slot.authority.roster.roster.authority_generation;
    this.slot.authority = state;
    this.store();
    const roster = state.roster.roster;
    const active = (id: string | null) =>
      id === null ||
      roster.members.some(
        (member) => member.device_id === id && !member.revoked,
      );
    if (
      roster.authority_generation !== generation ||
      !active(this.slot.source) ||
      !active(this.slot.target)
    )
      await this.shutdown("Membership changed");
  }
  private leg(socket: WebSocket): Leg | undefined {
    const value: unknown = socket.deserializeAttachment();
    if (
      !this.slot ||
      !object(value, [
        "side",
        "id",
        "outstanding",
        "pairingBytes",
        "traffic",
        "deadline",
      ]) ||
      (value.side !== "source" && value.side !== "target") ||
      value.id !==
        (value.side === "source" ? this.slot.sourceId : this.slot.targetId) ||
      typeof value.outstanding !== "number" ||
      !Number.isSafeInteger(value.outstanding) ||
      value.outstanding < 0 ||
      value.outstanding > WINDOW ||
      typeof value.pairingBytes !== "number" ||
      !Number.isSafeInteger(value.pairingBytes) ||
      value.pairingBytes < 0 ||
      (this.slot.pairing && value.pairingBytes > 64 * 1024) ||
      typeof value.deadline !== "number" ||
      !Number.isSafeInteger(value.deadline)
    )
      return;
    const traffic = trafficCounters(value.traffic);
    if (!traffic) return;
    return {
      side: value.side,
      id: value.id as string,
      outstanding: value.outstanding,
      pairingBytes: value.pairingBytes,
      deadline: value.deadline,
      traffic,
    };
  }
  private peer(side: string): WebSocket | undefined {
    return this.ctx.getWebSockets().find((socket) => {
      const leg = this.leg(socket);
      return leg && leg.side !== side && socket.readyState === WebSocket.OPEN;
    });
  }
  async fetch(request: Request): Promise<Response> {
    const match =
      /(?:\/networks\/|\/\.well-known\/xrun\/data\/)(net_[a-z2-7]{52})(?:\/data)?\/([a-f0-9]{32})\/(source|target)\/([a-f0-9]{32})$/.exec(
        new URL(request.url).pathname,
      );
    const slot = this.slot;
    if (
      !match ||
      !slot ||
      slot.closed ||
      slot.network !== match[1] ||
      slot.sid !== match[2] ||
      slot.deadline <= Date.now() ||
      match[4] !==
        (match[3] === "source" ? slot.sourceTicket : slot.targetTicket) ||
      (slot.warm && match[3] !== "target") ||
      this.ctx
        .getWebSockets()
        .some((socket) => this.leg(socket)?.side === match[3])
    )
      return new Response("Invalid data ticket", { status: 403 });
    requireFresh(slot.authority);
    const side = match[3] as Leg["side"];
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server);
    server.serializeAttachment({
      side,
      id: side === "source" ? slot.sourceId : slot.targetId,
      outstanding: 0,
      pairingBytes: 0,
      traffic: [],
      deadline: slot.warm
        ? slot.deadline
        : Math.min(slot.deadline, Date.now() + 10_000),
    } satisfies Leg);
    // Consume durably before releasing the upgrade. A repeated ticket cannot create another leg.
    if (side === "source") slot.sourceTicket = "";
    else slot.targetTicket = "";
    this.store();
    if (slot.warm) {
      server.send(JSON.stringify({ type: "standby_ready" }));
      await this.env.NETWORKS.getByName(RELAY_OBJECT).shardReady(
        slot.sid,
        slot.releaseTicket,
      );
    } else this.pairSockets();
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
  private pairSockets(): void {
    const sockets = this.ctx
      .getWebSockets()
      .filter(
        (socket) => socket.readyState === WebSocket.OPEN && this.leg(socket),
      );
    if (sockets.length !== 2 || !this.slot) return;
    for (const socket of sockets) {
      const leg = this.leg(socket);
      if (!leg) continue;
      leg.deadline = this.slot.pairing ? this.slot.deadline : Date.now() + IDLE;
      socket.serializeAttachment(leg);
      socket.send(JSON.stringify({ type: "connected", flow_control: true }));
    }
  }
  private count(leg: Leg, bytes: number, outgoing: boolean): void {
    const day = Math.floor(Date.now() / TRAFFIC_DAY) * TRAFFIC_DAY;
    let cell = leg.traffic.find((cell) => cell.day_ms === day);
    if (!cell) {
      cell = {
        day_ms: day,
        first_seen_ms: Date.now(),
        ingress_bytes: 0,
        egress_bytes: 0,
      };
      leg.traffic.push(cell);
    }
    const key = outgoing ? "egress_bytes" : "ingress_bytes";
    if (!Number.isSafeInteger(cell[key] + bytes)) {
      this.incomplete = true;
      return;
    }
    cell[key] += bytes;
    this.flushAt ??= Date.now() + 10_000;
  }
  private snapshots(closing?: WebSocket): TrafficLeg[] {
    if (!this.slot) return [];
    if (this.final) return this.final;
    const base = legs(this.slot);
    const sockets = [
      ...new Set([...this.ctx.getWebSockets(), ...(closing ? [closing] : [])]),
    ];
    return sockets.flatMap((socket) => {
      const leg = this.leg(socket);
      const binding = leg && base.find((binding) => binding.id === leg.id);
      return binding && leg ? [{ ...binding, traffic: leg.traffic }] : [];
    });
  }
  async snapshot(): Promise<{
    legs: TrafficLeg[];
    finished: boolean;
    incomplete: boolean;
  }> {
    return {
      legs: this.snapshots(),
      finished: this.slot?.closed ?? true,
      incomplete: this.incomplete || !this.slot,
    };
  }
  private async flush(finished = false): Promise<boolean> {
    if (!this.slot) return true;
    // An unclaimed callback has no counted business traffic or active accounting marker.
    if (this.slot.warm && !finished) return true;
    try {
      await this.env.NETWORKS.getByName(RELAY_OBJECT).shardCheckpoint(
        this.slot.sid,
        this.slot.releaseTicket,
        this.snapshots(),
        finished,
        this.incomplete,
      );
      this.flushAt = null;
      return true;
    } catch (error) {
      this.incomplete = true;
      this.flushAt = Date.now() + 5000;
      console.warn(
        "Data accounting unavailable; forwarding continues",
        String(error),
      );
      return false;
    }
  }
  private async schedule(): Promise<void> {
    if (!this.slot) return;
    const deadlines = this.ctx.getWebSockets().flatMap((socket) => {
      const leg = this.leg(socket);
      return leg ? [leg.deadline] : [];
    });
    const next = this.slot.closed
      ? this.final
        ? (this.flushAt ?? Date.now() + 5000)
        : Infinity
      : Math.min(
          this.slot.deadline,
          this.slot.authority.lease.lease.expires_at_ms,
          this.flushAt ?? Infinity,
          ...deadlines,
          Date.now() + 60_000,
        );
    if (!Number.isFinite(next)) return;
    if (this.alarmAt === null || next < this.alarmAt) {
      try {
        this.alarmAt = Math.max(Date.now() + 1, next);
        await this.ctx.storage.setAlarm(this.alarmAt);
      } catch (error) {
        this.alarmAt = null;
        this.incomplete = true;
        console.warn(
          "Data checkpoint alarm unavailable; forwarding continues",
          String(error),
        );
      }
    }
  }
  async alarm(): Promise<void> {
    this.alarmAt = null;
    if (!this.slot) return;
    if (this.slot.closed) {
      if (
        (await this.flush(true)) ||
        Date.now() >= this.slot.updated + TRAFFIC_DAY
      ) {
        this.final = null;
        await this.ctx.storage.deleteAll();
      }
      await this.schedule();
      return;
    }
    if (
      this.slot.deadline <= Date.now() ||
      this.slot.authority.lease.lease.expires_at_ms <= Date.now() ||
      this.ctx
        .getWebSockets()
        .some((socket) => (this.leg(socket)?.deadline ?? 0) <= Date.now())
    ) {
      await this.shutdown("Data channel expired");
      return;
    }
    await this.flush();
    if (!this.slot.pairing && !this.slot.warm)
      this.slot.deadline = Date.now() + IDLE;
    this.store();
    await this.schedule();
  }
  private async shutdown(reason: string, closing?: WebSocket): Promise<void> {
    if (!this.slot || this.slot.closed) return;
    this.final = this.snapshots(closing);
    this.slot.closed = true;
    this.slot.updated = Date.now();
    this.store();
    for (const socket of this.ctx.getWebSockets()) {
      try {
        socket.close(1000, reason);
      } catch {
        /* Already closed. */
      }
    }
    if (await this.flush(true)) {
      this.final = null;
      await this.ctx.storage.deleteAll();
    }
    this.alarmAt = null;
    await this.ctx.storage.deleteAlarm();
    await this.schedule();
  }
  async close(): Promise<void> {
    await this.shutdown("Admission closed");
  }
  async webSocketMessage(
    socket: WebSocket,
    message: string | ArrayBuffer,
  ): Promise<void> {
    const state = this.leg(socket);
    if (!state || !this.slot || this.slot.closed) return;
    try {
      requireFresh(this.slot.authority);
      if (typeof message === "string") {
        if (message.length > ACK_MESSAGE_BYTES)
          throw new Error("Oversized data control");
        const value: unknown = JSON.parse(message);
        if (!object(value)) throw new Error("Invalid data control");
        if (
          object(value, ["type", "idle"]) &&
          value.type === "cache_state" &&
          state.side === "source" &&
          typeof value.idle === "boolean"
        ) {
          await this.env.NETWORKS.getByName(RELAY_OBJECT).shardIdle(
            this.slot.sid,
            this.slot.releaseTicket,
            value.idle,
          );
          socket.send(
            JSON.stringify({ type: "cache_state", idle: value.idle }),
          );
          return;
        }
        const peer = this.peer(state.side);
        const remote = peer && this.leg(peer);
        if (
          !peer ||
          !remote ||
          !object(value, ["type", "bytes"]) ||
          value.type !== "ack" ||
          typeof value.bytes !== "number" ||
          !Number.isSafeInteger(value.bytes) ||
          value.bytes <= 0 ||
          value.bytes > remote.outstanding
        )
          throw new Error("Invalid data acknowledgment");
        remote.outstanding -= value.bytes;
        peer.serializeAttachment(remote);
        peer.send(message);
        return;
      }
      if (!message.byteLength || message.byteLength > FRAME)
        throw new Error("Ciphertext frame too large");
      if (
        this.slot.pairing &&
        (message.byteLength > 16 * 1024 ||
          state.pairingBytes + message.byteLength > 64 * 1024)
      )
        throw new Error("Pairing tunnel exceeds budget");
      const peer = this.peer(state.side);
      const remote = peer && this.leg(peer);
      if (!peer || !remote || state.outstanding + message.byteLength > WINDOW)
        throw new Error("Ciphertext window exceeded");
      const oldest =
        Math.floor(Date.now() / TRAFFIC_DAY) * TRAFFIC_DAY - TRAFFIC_DAY;
      if (
        [...state.traffic, ...remote.traffic].some(
          (cell) => cell.day_ms < oldest,
        )
      ) {
        await this.flush();
        state.traffic = state.traffic.filter((cell) => cell.day_ms >= oldest);
        remote.traffic = remote.traffic.filter((cell) => cell.day_ms >= oldest);
      }
      this.count(state, message.byteLength, false);
      state.pairingBytes += this.slot.pairing ? message.byteLength : 0;
      state.outstanding += message.byteLength;
      socket.serializeAttachment(state);
      peer.send(message);
      this.count(remote, message.byteLength, true);
      if (!this.slot.pairing)
        remote.deadline = state.deadline = Date.now() + IDLE;
      socket.serializeAttachment(state);
      peer.serializeAttachment(remote);
      if (
        this.flushAt !== null &&
        (this.alarmAt === null || this.flushAt < this.alarmAt)
      )
        await this.schedule();
    } catch (error) {
      console.warn("Data channel closed", String(error));
      await this.shutdown("Invalid data state", socket);
    }
  }
  async webSocketClose(socket: WebSocket): Promise<void> {
    await this.shutdown("Peer closed", socket);
  }
  async webSocketError(socket: WebSocket): Promise<void> {
    await this.shutdown("Peer failed", socket);
  }
}
