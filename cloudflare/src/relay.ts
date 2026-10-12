import { region } from "./regions";
import type { XrunData } from "./data";
import { Shards, dataPath, legs, type ShardSlot } from "./shards";
import { trafficCounters, type TrafficLeg } from "./traffic-state";
import { DurableObject } from "cloudflare:workers";
import { Admission, AdmissionError } from "./admission";
import { object, randomRoute, verifyProof, type VerifiedProof } from "./auth";
import { AUTHORITY_BYTES, requireFresh, verifyAuthority } from "./authority";
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
  SESSIONS_PER_SOURCE,
  MANAGER_RESERVED_SESSIONS,
  WINDOW,
} from "./limits";
import { route, type AttachRoute } from "./routes";
import { PROTOCOL } from "./protocol";
import { Pairings, pairingLocator } from "./pairing";
import { Traffic, trafficQuery } from "./traffic";
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
  DATA?: DurableObjectNamespace<XrunData>;
  XRUN_RELAY_REGION?: DurableObjectLocationHint;
  RELAY_ROUTE: string;
  XRUN_PROTOCOL_MIN: number;
  XRUN_PROTOCOL_MAX: number;
}
export function error(code: string, message: string, status = 400): Response {
  return Response.json({ type: "error", code, message }, { status });
}

/** Live routing bindings and ciphertext windows survive with hibernating sockets. */
export class XrunRelay extends DurableObject<Env> {
  private traffic: Traffic;
  private shards: Shards;
  private admission: Admission;
  private pairings: Pairings;
  private connections: Connections;
  private nextAlarm: number | null = null;
  private retryAuthorityAt: number | null = null;

  private data(sid: string) {
    if (!this.env.DATA) throw new Error("Missing data namespace");
    const hint = this.shards.get(sid)?.region ?? this.env.XRUN_RELAY_REGION;
    return this.env.DATA.getByName(
      sid,
      hint ? { locationHint: hint } : undefined,
    );
  }
  async dataTicket(
    network: string,
    sid: string,
    side: string,
    ticket: string,
  ): Promise<boolean> {
    const slot = this.shards.get(sid);
    const authority = this.admission.state();
    if (
      !slot ||
      !authority ||
      !this.shards.ticket(network, sid, side, ticket) ||
      this.connections.control(slot.target)?.state.generation !==
        slot.generation ||
      slot.authority.roster.roster.authority_generation !==
        authority.roster.roster.authority_generation
    )
      return false;
    try {
      requireFresh(authority);
    } catch {
      return false;
    }
    const active = (id: string | null) =>
      id === null ||
      authority.roster.roster.members.some(
        (member) => member.device_id === id && !member.revoked,
      );
    return active(slot.source) && active(slot.target);
  }
  async shardReady(sid: string, ticket: string): Promise<void> {
    const slot = this.shards.get(sid);
    if (!slot || slot.releaseTicket !== ticket || slot.closed || !slot.warm)
      throw new Error("Invalid warm registration");
    slot.ready = true;
    this.shards.save(slot);
  }
  async shardIdle(sid: string, ticket: string, idle: boolean): Promise<void> {
    const slot = this.shards.get(sid);
    if (!slot || slot.releaseTicket !== ticket || slot.closed || slot.warm)
      throw new Error("Invalid data admission");
    slot.cachedSince = idle ? Date.now() : null;
    if (!slot.pairing) slot.deadline = Date.now() + IDLE;
    this.shards.save(slot);
  }
  async shardCheckpoint(
    sid: string,
    ticket: string,
    snapshots: TrafficLeg[],
    finished: boolean,
    incomplete = false,
  ): Promise<void> {
    const slot = this.shards.get(sid);
    if (
      !slot ||
      slot.releaseTicket !== ticket ||
      snapshots.length > 2 ||
      new Set(snapshots.map((leg) => leg.id)).size !== snapshots.length
    )
      throw new Error("Unknown data accounting binding");
    const bindings = legs(slot);
    const safe = snapshots.map((leg) => {
      const binding = bindings.find((value) => value.id === leg.id);
      const counters = trafficCounters(leg.traffic);
      if (
        !binding ||
        !counters ||
        binding.network !== leg.network ||
        binding.role !== leg.role ||
        binding.source !== leg.source ||
        binding.target !== leg.target
      )
        throw new Error("Invalid data accounting");
      return { ...binding, traffic: counters };
    });
    if (incomplete)
      this.traffic.markIncomplete(
        "Data channel recovery lost a peer or a checkpoint",
      );
    if (!this.traffic.checkpoint(safe))
      throw new Error("Traffic checkpoint unavailable");
    slot.closed ||= finished;
    if (finished) {
      slot.closePending = false;
      slot.accounted = true;
    }
    if (!slot.pairing) slot.deadline = Date.now() + IDLE;
    this.shards.save(slot);
  }
  private closeShard(slot: ShardSlot): void {
    slot.closed = true;
    slot.closePending = true;
    this.shards.save(slot);
    this.ctx.waitUntil(
      this.data(slot.sid)
        .close()
        .then(() => {
          const current = this.shards.get(slot.sid);
          if (current) {
            current.closePending = false;
            this.shards.save(current);
          }
        })
        .catch(async () => {
          this.retryAuthorityAt = Date.now() + 5000;
          await this.schedule();
        }),
    );
  }
  private async refreshShards(): Promise<void> {
    if (!this.env.DATA) return;
    for (const slot of this.shards
      .all()
      .filter((slot) => !slot.warm && !slot.accounted)) {
      try {
        const snapshot = await this.data(slot.sid).snapshot();
        if (
          snapshot.legs.length !== 2 &&
          !snapshot.finished &&
          slot.deadline <= Date.now()
        )
          this.traffic.markIncomplete("Data peer missing");
        await this.shardCheckpoint(
          slot.sid,
          slot.releaseTicket,
          snapshot.legs,
          snapshot.finished,
          snapshot.incomplete,
        );
      } catch {
        this.traffic.markIncomplete("Data shard snapshot unavailable");
      }
    }
  }
  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.admission = new Admission(ctx.storage);
    this.shards = new Shards(ctx.storage);
    this.pairings = new Pairings(ctx.storage);
    this.traffic = new Traffic(ctx.storage);
    this.connections = new Connections(ctx.getWebSockets());
    this.validateRestoredBindings();
    this.traffic.recover(this.trafficLegs(), this.shards.ids());
    ctx.blockConcurrencyWhile(async () => {
      await this.admission.validate();
      this.nextAlarm = await ctx.storage.getAlarm();
      this.publishAuthority();
      await this.schedule();
    });
  }
  /** The same restoration path is used after eviction and by workerd recovery tests. */
  protected async restoreSockets(): Promise<void> {
    this.admission = new Admission(this.ctx.storage);
    this.pairings = new Pairings(this.ctx.storage);
    this.connections = new Connections(this.ctx.getWebSockets());
    this.traffic = new Traffic(this.ctx.storage);
    await this.admission.validate();
    this.validateRestoredBindings();
    this.traffic.recover(this.trafficLegs(), this.shards.ids());
    this.publishAuthority();
  }
  private validateRestoredBindings(): void {
    for (const { socket, state } of [...this.connections.all()]) {
      if (
        state.role !== "auth" &&
        state.role !== "verifying" &&
        this.admission.owner()?.network_id !== state.network
      ) {
        this.close(socket, 1008, "Restored network is not admitted");
        continue;
      }
      if (!session(state) && state.role !== "standby") continue;
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
        peer.sid === state.sid &&
        peer.pairing === state.pairing &&
        (!state.pairing || control.state.manager);
      if (!bound) this.close(socket, 1008, "Restored tunnel binding missing");
    }
  }
  private publishAuthority(retryOnly = false): void {
    const authority = this.admission.state();
    this.retryAuthorityAt = null;
    if (!authority) return;
    const roster = authority.roster.roster;
    const active = (device: string) =>
      roster.members.some((m) => m.device_id === device && !m.revoked);
    if (this.env.DATA) {
      for (const slot of this.shards.all()) {
        if (slot.closePending) {
          this.closeShard(slot);
          continue;
        }
        if (slot.closed) continue;
        if (
          slot.authority.roster.roster.authority_generation !==
            roster.authority_generation ||
          !active(slot.target) ||
          (slot.source !== null && !active(slot.source))
        ) {
          this.closeShard(slot);
          continue;
        }
        if (
          slot.authority.lease.lease.issued_at_ms >=
            authority.lease.lease.issued_at_ms &&
          slot.authority.roster.roster.version >= roster.version
        )
          continue;
        this.ctx.waitUntil(
          this.data(slot.sid)
            .authority(authority)
            .then(() => {
              // Merge only the acknowledged lease; other events may have closed or idled this admission meanwhile.
              const current = this.shards.get(slot.sid);
              if (
                current &&
                (current.authority.roster.roster.version < roster.version ||
                  (current.authority.roster.roster.version === roster.version &&
                    current.authority.lease.lease.issued_at_ms <=
                      authority.lease.lease.issued_at_ms))
              ) {
                current.authority = authority;
                this.shards.save(current);
              }
            })
            .catch(async () => {
              this.retryAuthorityAt = Date.now() + 5000;
              await this.schedule();
            }),
        );
      }
    }
    const pending = new Set(this.admission.pending());
    for (const { socket, state } of [...this.connections.all()]) {
      if (state.role === "control") {
        if (
          !active(state.device) ||
          state.authorityGeneration !== roster.authority_generation ||
          state.manager !== (state.device === roster.manager_id)
        ) {
          this.close(socket, 1008, "Membership changed");
        } else if (!retryOnly || pending.has(state.device)) {
          try {
            this.send(socket, { type: "authority", state: authority });
          } catch {
            this.close(socket, 1011, "Authority delivery failed");
          }
          if (pending.has(state.device))
            this.retryAuthorityAt = Date.now() + 5000;
        }
      } else if (
        state.role === "standby" &&
        (state.authorityGeneration !== roster.authority_generation ||
          !active(state.target))
      ) {
        this.close(socket, 1008, "Membership changed");
      } else if (
        session(state) &&
        (state.authorityGeneration !== roster.authority_generation ||
          !active(state.target) ||
          (state.role !== "target" &&
            state.source !== null &&
            !active(state.source)))
      ) {
        this.close(socket, 1008, "Membership changed");
      } else if (
        state.role === "traffic" &&
        (!active(state.device) ||
          state.authorityGeneration !== roster.authority_generation)
      ) {
        this.close(socket, 1008, "Membership changed");
      }
    }
  }
  private send(ws: WebSocket, value: object): void {
    ws.send(JSON.stringify(value));
  }
  private trafficLegs(): TunnelState[] {
    return [...this.connections.all()].flatMap(({ state }) =>
      tunnel(state) ? [state] : [],
    );
  }
  private close(
    ws: WebSocket,
    code = 1000,
    reason = "Connection closed",
  ): void {
    const state = this.connections.get(ws)?.state;
    if (!state) return;
    if (tunnel(state)) this.traffic.finish(state);
    if (state.role === "control" && this.env.DATA) {
      for (const slot of this.shards
        .all()
        .filter(
          (slot) =>
            !slot.closed &&
            slot.target === state.device &&
            slot.generation === state.generation,
        )) {
        this.closeShard(slot);
      }
    }
    // Capture bindings before discarding all role-specific fields.
    const peer = tunnel(state)
      ? this.connections.find(state.peer)?.socket
      : undefined;
    const bound =
      state.role === "control"
        ? this.connections.bound(state.device, state.generation)
        : [];
    this.connections.save(ws, {
      id: state.id,
      network: state.network,
      ip: state.ip,
      protocol: state.protocol,
      role: "closed",
    });
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
    let next: number | null = this.retryAuthorityAt;
    if (
      this.traffic.nextFlush !== null &&
      (next === null || this.traffic.nextFlush < next)
    )
      next = this.traffic.nextFlush;
    for (const { state } of this.connections.all()) {
      if ("deadline" in state && (next === null || state.deadline < next))
        next = state.deadline;
    }
    for (const slot of this.shards.all()) {
      const deadline = slot.closed ? slot.updated + 86_400_000 : slot.deadline;
      if (next === null || deadline < next) next = deadline;
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
    this.shards.prune();
    if (this.traffic.nextFlush !== null && this.traffic.nextFlush <= now)
      this.traffic.flush(this.trafficLegs());
    if (this.retryAuthorityAt !== null && this.retryAuthorityAt <= now)
      this.publishAuthority(true);
    if (this.env.DATA) {
      for (const slot of this.shards
        .all()
        .filter((slot) => !slot.closed && slot.deadline <= now)) {
        this.closeShard(slot);
      }
    }
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
    const pathname = new URL(request.url).pathname;
    const locator = pairingLocator(pathname);
    const pairing = locator ? this.pairings.find(locator) : undefined;
    const owner = this.admission.owner();
    if (locator && (!pairing || !owner))
      return error("INVALID_CODE", "Access code unavailable or expired", 404);
    const parsed =
      pairing && owner
        ? {
            action: "connect" as const,
            network: owner.network_id,
            target: owner.manager_id,
            path: `/networks/${owner.network_id}/connect/${owner.manager_id}`,
          }
        : route(pathname, this.env.RELAY_ROUTE);
    if (!parsed) return new Response("Not found", { status: 404 });
    const probe = parsed.network === "probe" && parsed.action === "status";
    try {
      if (!probe)
        this.admission.check(
          parsed.network,
          parsed.action === "claim" || parsed.action === "control",
        );
    } catch (reason) {
      if (reason instanceof AdmissionError)
        return error(reason.code, reason.message, 403);
      throw reason;
    }
    if (this.connections.size >= CONNECTION_LIMIT)
      return error("CONNECTION_LIMIT", "Too many connections", 429);
    const ip = request.headers.get("X-Xrun-Peer") || "unknown";
    if (locator) {
      try {
        this.pairings.throttlePeer(ip);
      } catch (reason) {
        if (reason instanceof AdmissionError)
          return error(reason.code, reason.message, 429);
        return error(
          "STORAGE_ERROR",
          "Pairing throttle storage unavailable",
          503,
        );
      }
    }
    if (
      parsed.action !== "attach" &&
      this.connections.authenticating(ip) >= AUTHENTICATING_PER_IP
    ) {
      return error("CONNECTION_LIMIT", "Too many unverified connections", 429);
    }
    if (this.env.DATA && parsed.action === "attach") {
      const slot = this.shards.get(parsed.sid);
      const control = this.connections.control(parsed.target);
      if (
        !slot ||
        slot.closed ||
        slot.target !== parsed.target ||
        slot.generation !== parsed.generation ||
        control?.state.generation !== parsed.generation ||
        slot.deadline <= Date.now()
      )
        return error("INVALID_SESSION", "Data reservation unavailable");
      const [client, server] = Object.values(new WebSocketPair());
      server.accept();
      server.send(
        JSON.stringify({ type: "data_route", path: dataPath(slot, "target") }),
      );
      server.close();
      return new Response(null, {
        status: 101,
        webSocket: client,
        headers: { "X-Xrun-Protocol": String(PROTOCOL.max) },
      });
    }
    const pending =
      parsed.action === "attach" ? this.pending(parsed) : undefined;
    if (parsed.action === "attach" && !pending)
      return error(
        "INVALID_SESSION",
        "Session missing, expired or already claimed",
      );
    const [client, server] = Object.values(new WebSocketPair());
    const protocol = Number(request.headers.get("X-Xrun-Negotiated-Protocol"));
    if (
      !Number.isInteger(protocol) ||
      protocol < PROTOCOL.min ||
      protocol > PROTOCOL.max
    )
      return error(
        "VERSION_MISMATCH",
        "Missing negotiated relay protocol",
        409,
      );
    const connection = {
      id: crypto.randomUUID(),
      network: parsed.network,
      ip,
      protocol,
      ...(region(request.headers.get("X-Xrun-Region"))
        ? { region: region(request.headers.get("X-Xrun-Region")) }
        : {}),
      ...(pairing ? { pairing: pairing.invitation_id } : {}),
    };
    this.ctx.acceptWebSocket(server);
    if (pending) {
      const source = pending.state;
      const binding = {
        target: source.target,
        generation: source.generation,
        sid: source.sid,
        authorityGeneration: source.authorityGeneration,
        ...(source.pairing ? { pairing: source.pairing } : {}),
      };
      const deadline = Date.now() + (source.pairing ? 20_000 : IDLE);
      this.connections.save(pending.socket, {
        ...source,
        role: "source",
        peer: connection.id,
        outstanding: 0,
        cachedSince: null,
        traffic: [],
        deadline,
      });
      this.connections.save(server, {
        ...connection,
        ...binding,
        role: "target",
        peer: source.id,
        outstanding: 0,
        deadline,
        traffic: [],
      });
      for (const socket of [pending.socket, server]) {
        const leg = this.connections.get(socket)?.state;
        if (leg && tunnel(leg)) this.traffic.begin(leg);
      }
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
      if (pairing)
        this.send(server, {
          type: "pairing_info",
          invitation_id: pairing.invitation_id,
          locator,
        });
      this.send(server, {
        type: "challenge",
        nonce,
        authority: pairing ? null : this.admission.state(),
      });
      if (probe) {
        this.send(server, {
          type: "relay_info",
          owner: this.admission.owner(),
        });
        this.close(server);
      }
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
  ): Promise<VerifiedProof | undefined> {
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
    if (state.pairing && proof)
      throw new Error("Pairing requires anonymous PAKE enrollment");
    const current = this.connections.get(ws)?.state;
    const sameChallenge =
      current?.role === "verifying" &&
      current.id === state.id &&
      current.nonce === state.nonce;
    if (ws.readyState !== WebSocket.OPEN || !sameChallenge) return;
    if (expired(current, Date.now()))
      throw new Error("Authentication timed out");
    if (proof && (this.admission.owner() || proof.manager)) {
      if (this.admission.authenticate(proof)) this.publishAuthority();
    }
    if (state.action === "claim") {
      if (!proof)
        throw new AdmissionError("NOT_MANAGER", "Manager proof required");
      const owner = this.admission.claim(
        state.network,
        proof.device,
        proof.manager,
      );
      this.send(ws, { type: "relay_info", owner });
      this.close(ws);
      return;
    }
    // A claim may have completed while this proof was being verified.
    if (state.action === "control" && proof?.manager && !this.admission.owner())
      this.admission.claim(state.network, proof.device, true);
    this.admission.check(state.network, false);
    if (proof)
      proof.manager = this.admission.manager(proof.device, proof.manager);
    if (state.action === "status") {
      if (!proof) throw new Error("Member proof required");
      this.send(ws, {
        type: "status",
        devices: this.connections.devices(),
        pending_authority: proof.manager ? this.admission.pending() : [],
      });
      this.close(ws);
    } else if (state.action === "standby") {
      if (this.env.DATA) {
        const control = this.connections.control(state.target);
        if (
          !proof ||
          proof.device !== state.target ||
          control?.state.generation !== state.generation
        )
          throw new Error("Standby control mismatch");
        const warm = this.shards
          .all()
          .filter(
            (slot) => slot.warm && !slot.closed && slot.deadline > Date.now(),
          );
        if (
          warm.length >= 32 ||
          warm.some(
            (slot) =>
              slot.target === state.target &&
              slot.generation === state.generation,
          )
        )
          throw new Error("Standby capacity reached");
        const authority = this.admission.state();
        if (!authority) throw new Error("Missing authority");
        const slot = this.shards.create(
          state.network,
          state.target,
          state.generation,
          null,
          state.ip,
          false,
          false,
          authority,
          true,
        );
        slot.region = state.region;
        this.shards.prune();
        this.shards.save(slot);
        await this.data(slot.sid).prepare(slot);
        this.send(ws, { type: "data_route", path: dataPath(slot, "target") });
        this.close(ws);
        await this.schedule();
        return;
      }
      const control = this.connections.control(state.target);
      if (
        !proof ||
        proof.device !== state.target ||
        control?.state.generation !== state.generation
      )
        throw new Error("Standby control binding mismatch");
      const callbacks = [...this.connections.all()].filter(
        ({ state }) => state.role === "standby",
      );
      if (
        callbacks.length >= 32 ||
        callbacks.some(
          ({ state: other }) =>
            other.role === "standby" && other.target === state.target,
        )
      )
        throw new Error("Standby callback capacity reached");
      this.connections.save(ws, {
        ...base(state),
        role: "standby",
        target: state.target,
        generation: state.generation,
        authorityGeneration: proof.authorityGeneration,
        deadline: Date.now() + IDLE,
      });
      this.send(ws, { type: "standby_ready" });
      await this.schedule();
    } else if (state.action === "traffic") {
      if (!proof) throw new Error("Member proof required");
      this.connections.save(ws, {
        ...base(state),
        role: "traffic",
        device: proof.device,
        manager: proof.manager,
        deadline: Date.now() + AUTH_TIMEOUT_MS,
        authorityGeneration: proof.authorityGeneration,
      });
      this.send(ws, { type: "traffic_ready" });
    } else if (state.action === "control") {
      if (!proof) throw new Error("Member proof required");
      this.registerControl(ws, state, proof);
    } else {
      await this.startSession(ws, state, proof);
    }
  }
  private registerControl(
    ws: WebSocket,
    state: ChallengeState,
    proof: VerifiedProof,
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
      authorityGeneration: proof.authorityGeneration,
    });
    this.send(ws, { type: "hello_ack", generation });
    const authority = this.admission.state();
    if (authority) this.send(ws, { type: "authority", state: authority });
    this.retryAuthorityAt = Date.now() + 5000;
  }
  private async startSession(
    ws: WebSocket,
    state: ChallengeState & { action: "connect"; target: string },
    proof: VerifiedProof | undefined,
  ): Promise<void> {
    const target = this.connections.control(state.target);
    if (!target) {
      this.reject(ws, "DEVICE_OFFLINE", "Target is offline");
      return;
    }
    if (!proof && !target.state.manager)
      throw new Error("Member proof required");
    const authority = this.admission.state();
    if (!authority) throw new Error("Membership freshness is missing");
    requireFresh(authority);
    if (
      target.state.authorityGeneration !==
      authority.roster.roster.authority_generation
    )
      throw new Error("Target uses old management generation");
    const source = proof?.device ?? null;
    // The manager's authenticated live control already carries its root proof.
    // Anonymous pairing cannot consume this last protected network slot.
    const management =
      proof !== undefined &&
      (proof.manager ||
        this.connections.control(proof.device)?.state.manager === true);
    const anonymousFull =
      !proof &&
      this.connections.anonymous(state.target) +
        this.shards
          .all()
          .filter(
            (slot) =>
              !slot.warm &&
              !slot.closed &&
              slot.source === null &&
              slot.target === state.target &&
              slot.deadline > Date.now(),
          ).length >=
        ANONYMOUS_PER_TARGET;
    if (anonymousFull) {
      this.reject(ws, "SESSION_LIMIT", "Too many concurrent relay sessions");
      return;
    }
    for (;;) {
      const sourceFull =
        this.connections.source(source, state.ip) >= SESSIONS_PER_SOURCE;
      const ordinaryFull =
        !management &&
        this.connections.ordinarySessions >=
          SESSIONS - MANAGER_RESERVED_SESSIONS;
      const totalFull = this.connections.sourceCount >= SESSIONS;
      if (!sourceFull && !ordinaryFull && !totalFull) break;
      const cached = this.connections.oldestCached((candidate) => {
        if (sourceFull) {
          return (
            candidate.source === source &&
            (source !== null || candidate.ip === state.ip)
          );
        }
        return !ordinaryFull || !candidate.management;
      });
      if (!cached) {
        this.reject(ws, "SESSION_LIMIT", "Too many concurrent relay sessions");
        return;
      }
      this.close(cached.socket, 1000, "Cached session reclaimed");
    }
    if (this.env.DATA) {
      const warm =
        proof && !state.pairing
          ? this.shards
              .all()
              .find(
                (slot) =>
                  slot.warm &&
                  slot.ready &&
                  !slot.closed &&
                  slot.target === state.target &&
                  slot.generation === target.state.generation &&
                  slot.deadline > Date.now(),
              )
          : undefined;
      const slot =
        warm ??
        this.shards.create(
          state.network,
          state.target,
          target.state.generation,
          source,
          state.ip,
          management,
          !!state.pairing,
          authority,
        );
      slot.region ??= state.region;
      slot.warm = false;
      slot.source = source;
      slot.ip = state.ip;
      slot.management = management;
      slot.pairing = !!state.pairing;
      slot.authority = authority;
      slot.deadline = Date.now() + (slot.pairing ? 20_000 : IDLE);
      let reclaimed: ShardSlot[];
      try {
        reclaimed = this.shards.reserve(slot);
      } catch {
        this.reject(ws, "SESSION_LIMIT", "Too many concurrent data channels");
        return;
      }
      for (const old of reclaimed) this.closeShard(old);
      try {
        if (warm) await this.data(slot.sid).assign(slot);
        else await this.data(slot.sid).prepare(slot);
      } catch (error) {
        this.closeShard(slot);
        throw error;
      }
      if (
        ws.readyState !== WebSocket.OPEN ||
        this.connections.control(state.target)?.state.generation !==
          slot.generation
      ) {
        this.closeShard(slot);
        return;
      }
      for (const leg of legs(slot)) this.traffic.begin(leg);
      if (!warm)
        this.send(
          target.socket,
          state.pairing
            ? {
                type: "pairing_incoming",
                session_id: slot.sid,
                invitation_id: state.pairing,
              }
            : { type: "incoming", session_id: slot.sid },
        );
      this.send(ws, {
        type: "data_route",
        path: dataPath(slot, "source", !!state.pairing),
      });
      this.close(ws);
      await this.schedule();
      return;
    }
    const sid = crypto.randomUUID().replaceAll("-", "");
    this.connections.save(ws, {
      ...base(state),
      role: "pending",
      target: state.target,
      generation: target.state.generation,
      authorityGeneration: authority.roster.roster.authority_generation,
      sid,
      anonymous: !proof,
      source,
      management,
      deadline: Date.now() + CONNECT_TIMEOUT_MS,
    });
    const warm =
      proof && !state.pairing
        ? [...this.connections.all()].find(
            ({ state: entry }) =>
              entry.role === "standby" &&
              entry.target === state.target &&
              entry.generation === target.state.generation,
          )
        : undefined;
    if (warm) {
      const pending = this.connections.get(ws)?.state;
      if (pending?.role !== "pending")
        throw new Error("Missing source reservation");
      const deadline = Date.now() + IDLE;
      this.connections.save(ws, {
        ...pending,
        role: "source",
        peer: warm.state.id,
        outstanding: 0,
        cachedSince: null,
        traffic: [],
        deadline,
      });
      this.connections.save(warm.socket, {
        ...base(warm.state),
        role: "target",
        target: pending.target,
        generation: pending.generation,
        sid,
        authorityGeneration: pending.authorityGeneration,
        peer: pending.id,
        outstanding: 0,
        traffic: [],
        deadline,
      });
      for (const socket of [ws, warm.socket]) {
        const state = this.connections.get(socket)?.state;
        if (state && tunnel(state)) this.traffic.begin(state);
        this.send(socket, { type: "connected", flow_control: true });
      }
      return;
    }
    this.send(
      target.socket,
      state.pairing
        ? {
            type: "pairing_incoming",
            session_id: sid,
            invitation_id: state.pairing,
          }
        : { type: "incoming", session_id: sid },
    );
  }
  private async controlMessage(
    ws: WebSocket,
    state: ControlState,
    message: string | ArrayBuffer,
  ): Promise<void> {
    if (typeof message !== "string" || message.length > AUTHORITY_BYTES)
      throw new Error("Invalid control message");
    const value: unknown = JSON.parse(message);
    if (object(value) && value.type === "authority") {
      if (!state.manager || this.admission.owner()?.manager_id !== state.device)
        throw new AdmissionError(
          "NOT_MANAGER",
          "Only the current manager can publish authority",
        );
      const authority = await verifyAuthority(value.state, state.network);
      if (
        authority.roster.roster.manager_id !== state.device ||
        authority.roster.roster.authority_generation !==
          state.authorityGeneration
      )
        throw new Error("Control and authority differ");
      if (this.admission.publish(authority)) this.publishAuthority();
      this.send(ws, {
        type: "authority_ack",
        authority_generation: authority.lease.lease.authority_generation,
        version: authority.lease.lease.version,
        hash: authority.lease.lease.hash,
      });
      return;
    }
    if (message.length > CONTROL_MESSAGE_BYTES)
      throw new Error("Invalid control message size");
    if (object(value) && value.type === "authority_ack") {
      if (
        typeof value.authority_generation !== "number" ||
        typeof value.version !== "number" ||
        typeof value.hash !== "string"
      )
        throw new Error("Invalid authority acknowledgement");
      this.admission.ack(
        state.device,
        value.authority_generation,
        value.version,
        value.hash,
      );
      this.publishAuthority(true);
      return;
    }
    if (
      object(value) &&
      (value.type === "pairing_register" || value.type === "pairing_cancel")
    ) {
      if (!state.manager)
        throw new AdmissionError(
          "NOT_MANAGER",
          "Pairing requires the proven manager",
        );
      if (
        typeof value.invitation_id !== "string" ||
        !/^[a-f0-9]{32}$/.test(value.invitation_id)
      )
        throw new Error("Invalid invitation identity");
      try {
        if (value.type === "pairing_cancel")
          this.pairings.cancel(value.invitation_id);
        else {
          if (
            !object(value, [
              "type",
              "invitation_id",
              "locator",
              "expires_at",
            ]) ||
            typeof value.locator !== "string" ||
            typeof value.expires_at !== "number"
          )
            throw new AdmissionError(
              "INVALID_REQUEST",
              "Invalid pairing metadata",
            );
          this.pairings.register(
            value.invitation_id,
            value.locator,
            value.expires_at,
          );
        }
        this.send(ws, {
          type: "pairing_registered",
          invitation_id: value.invitation_id,
          error: null,
        });
      } catch (reason) {
        if (!(reason instanceof AdmissionError)) throw reason;
        this.send(ws, {
          type: "pairing_registered",
          invitation_id: value.invitation_id,
          error: { type: "error", code: reason.code, message: reason.message },
        });
      }
      return;
    }
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
      if (object(value, ["type", "idle"]) && value.type === "cache_state") {
        if (
          state.role !== "source" ||
          state.anonymous ||
          typeof value.idle !== "boolean"
        )
          throw new Error("Invalid cache state declaration");
        this.connections.save(ws, {
          ...state,
          cachedSince: value.idle ? Date.now() : null,
          deadline: Date.now() + IDLE,
        });
        this.send(ws, { type: "cache_state", idle: value.idle });
        return;
      }
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
        state.pairing &&
        (message.byteLength > 16 * 1024 ||
          state.outstanding + message.byteLength > 64 * 1024)
      ) {
        this.reject(ws, "MESSAGE_TOO_LARGE", "Pairing tunnel exceeds budget");
        return;
      }
      // Idle is a reclaimable hint. Encrypted multiplexing control frames may
      // arrive after the last application stream has ended.
      if (
        !message.byteLength ||
        message.byteLength > FRAME ||
        state.outstanding + message.byteLength > WINDOW
      ) {
        this.reject(ws, "MESSAGE_TOO_LARGE", "Ciphertext window exceeded");
        return;
      }
      state = {
        ...state,
        outstanding: state.outstanding + message.byteLength,
        traffic: this.traffic.recordLeg(state, message.byteLength, false),
      };
    }
    // Budget changes must always survive eviction. Persist the exact idle expiry
    // in the same write, so wake-up cannot expire a recently active peer early.
    this.connections.save(ws, {
      ...state,
      deadline: state.pairing ? state.deadline : Date.now() + IDLE,
    });
    peer.socket.send(message);
    if (typeof message !== "string") {
      this.connections.save(peer.socket, {
        ...peer.state,
        traffic: this.traffic.recordLeg(peer.state, message.byteLength, true),
      });
    }
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
      } else if (state.role === "traffic") {
        if (
          typeof message !== "string" ||
          message.length > CONTROL_MESSAGE_BYTES ||
          expired(state, Date.now())
        )
          throw new Error("Invalid traffic query");
        const value: unknown = JSON.parse(message);
        const query =
          object(value) && value.type === "traffic_query"
            ? trafficQuery(value.query)
            : undefined;
        if (!query) {
          this.reject(
            ws,
            "INVALID_REQUEST",
            "Invalid traffic period or pagination",
          );
          return;
        }
        try {
          await this.refreshShards();
          const report = this.traffic.report(
            state.network,
            state.manager ? null : state.device,
            query,
            Date.now(),
            this.trafficLegs(),
          );
          this.send(ws, { type: "traffic", report });
          this.close(ws);
        } catch {
          this.reject(ws, "STORAGE_ERROR", "Relay traffic storage unavailable");
        }
      } else if (state.role === "control") {
        await this.controlMessage(ws, state, message);
      } else if (tunnel(state)) {
        this.tunnelMessage(ws, state, message);
        // Extending an idle expiry cannot bring the next alarm forward. No
        // connection enumeration or storage read is needed for ordinary frames.
        if (
          this.traffic.nextFlush !== null &&
          (this.nextAlarm === null || this.nextAlarm > this.traffic.nextFlush)
        )
          await this.schedule();
        return;
      } else {
        throw new Error("Unexpected relay message");
      }
    } catch (reason) {
      if (!this.connections.get(ws)) return;
      if (reason instanceof AdmissionError) {
        this.reject(ws, reason.code, reason.message);
        await this.schedule();
        return;
      }
      if (
        reason instanceof Error &&
        reason.message.startsWith("NOT_MANAGER:")
      ) {
        this.reject(
          ws,
          (state.role === "auth" || state.role === "verifying") &&
            state.action === "claim"
            ? "RELAY_CLAIMED"
            : "NOT_MANAGER",
          "Manager proof differs from current authority",
        );
        await this.schedule();
        return;
      }
      if (reason instanceof Error) {
        const code = reason.message.split(":", 1)[0];
        if (
          ["ROSTER_EXPIRED", "ROSTER_ROLLBACK", "DEVICE_REVOKED"].includes(code)
        ) {
          this.reject(ws, code, reason.message);
          await this.schedule();
          return;
        }
      }
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
