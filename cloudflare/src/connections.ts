import {
  attachment,
  session,
  tunnel,
  type Attachment,
  type ControlState,
  type PendingState,
  type SourceState,
} from "./state";

export interface Connection<State extends Attachment = Attachment> {
  socket: WebSocket;
  state: State;
}

const NEXT_ROLES: Record<Attachment["role"], readonly Attachment["role"][]> = {
  auth: ["verifying", "closed"],
  verifying: ["control", "pending", "closed"],
  control: ["closed"],
  pending: ["source", "closed"],
  source: ["source", "closed"],
  target: ["target", "closed"],
  closed: [],
};

/** Indexes and serialized state change together, through save/remove only. */
export class Connections {
  private readonly bySocket = new Map<WebSocket, Connection>();
  private readonly byId = new Map<string, Connection>();
  private readonly controls = new Map<string, Connection>();
  private readonly pending = new Map<string, Connection>();
  private readonly bindings = new Map<string, Set<WebSocket>>();
  private readonly sources = new Set<WebSocket>();

  constructor(sockets: WebSocket[]) {
    for (const socket of sockets) {
      const state = attachment(socket.deserializeAttachment());
      if (
        state &&
        state.role !== "closed" &&
        socket.readyState === WebSocket.OPEN
      ) {
        const duplicate =
          this.find(state.id) ||
          (state.role === "control"
            ? this.controls.get(state.device)
            : undefined) ||
          (state.role === "pending" ? this.pending.get(state.sid) : undefined);
        if (!duplicate) {
          this.add(socket, state);
          continue;
        }
        this.remove(duplicate.socket);
        try {
          duplicate.socket.close(1008, "Duplicate restored binding");
        } catch {
          /* Already closed. */
        }
      }
      try {
        socket.close(1008, "Invalid restored state");
      } catch {
        /* Already closed. */
      }
    }
  }
  get size(): number {
    return this.bySocket.size;
  }
  get sourceCount(): number {
    return this.sources.size;
  }
  get controlCount(): number {
    return this.controls.size;
  }
  oldestCached(
    eligible: (state: SourceState) => boolean = () => true,
  ): Connection<SourceState> | undefined {
    let oldest: Connection<SourceState> | undefined;
    for (const socket of this.sources) {
      const connection = this.get(socket);
      if (
        connection?.state.role !== "source" ||
        connection.state.cachedSince === null ||
        !eligible(connection.state)
      )
        continue;
      if (
        !oldest ||
        connection.state.cachedSince < (oldest.state.cachedSince ?? Infinity)
      ) {
        oldest = { socket, state: connection.state };
      }
    }
    return oldest;
  }
  get(socket: WebSocket): Connection | undefined {
    return this.bySocket.get(socket);
  }
  find(id: string): Connection | undefined {
    return this.byId.get(id);
  }
  all(): IterableIterator<Connection> {
    return this.bySocket.values();
  }
  control(device: string): Connection<ControlState> | undefined {
    const connection = this.controls.get(device);
    if (connection?.state.role === "control")
      return { socket: connection.socket, state: connection.state };
  }
  claim(
    target: string,
    generation: string,
    sid: string,
    now: number,
  ): Connection<PendingState> | undefined {
    const connection = this.pending.get(sid);
    if (connection?.state.role !== "pending") return;
    const state = connection.state;
    if (
      state.target === target &&
      state.generation === generation &&
      state.deadline > now
    ) {
      return { socket: connection.socket, state };
    }
  }
  bound(target: string, generation: string): WebSocket[] {
    return [...(this.bindings.get(`${target}/${generation}`) || [])];
  }
  authenticating(ip: string): number {
    let count = 0;
    for (const { state } of this.all()) {
      if (
        (state.role === "auth" || state.role === "verifying") &&
        state.ip === ip
      )
        count++;
    }
    return count;
  }
  anonymous(target: string): number {
    let count = 0;
    for (const socket of this.sources) {
      const state = this.get(socket)?.state;
      if (
        state &&
        (state.role === "pending" || state.role === "source") &&
        state.target === target &&
        state.anonymous
      )
        count++;
    }
    return count;
  }
  source(source: string | null, ip: string): number {
    let count = 0;
    for (const socket of this.sources) {
      const state = this.get(socket)?.state;
      if (
        state &&
        (state.role === "pending" || state.role === "source") &&
        state.source === source &&
        (source !== null || state.ip === ip)
      ) count++;
    }
    return count;
  }
  get ordinarySessions(): number {
    let count = 0;
    for (const socket of this.sources) {
      const state = this.get(socket)?.state;
      if (
        state &&
        (state.role === "pending" || state.role === "source") &&
        !state.management
      ) count++;
    }
    return count;
  }
  devices(): string[] {
    return [...this.controls.keys()].sort();
  }
  save(socket: WebSocket, state: Attachment): void {
    const previous = this.get(socket);
    const validRole = previous
      ? NEXT_ROLES[previous.state.role].includes(state.role)
      : state.role === "auth" || state.role === "target";
    const sameIdentity =
      !previous ||
      (previous.state.id === state.id &&
        previous.state.network === state.network &&
        previous.state.ip === state.ip &&
        previous.state.protocol === state.protocol);
    if (!validRole || !sameIdentity)
      throw new Error("Invalid relay state transition");
    const prior = previous?.state;
    if (
      prior &&
      session(prior) &&
      session(state) &&
      (prior.target !== state.target ||
        prior.generation !== state.generation ||
        prior.sid !== state.sid)
    ) {
      throw new Error("Session binding is immutable");
    }
    if (prior && tunnel(prior) && tunnel(state) && prior.peer !== state.peer) {
      throw new Error("Tunnel peer is immutable");
    }
    if (
      prior &&
      (prior.role === "pending" || prior.role === "source") &&
      (state.role === "pending" || state.role === "source") &&
      (prior.anonymous !== state.anonymous ||
        prior.source !== state.source ||
        prior.management !== state.management)
    ) {
      throw new Error("Session admission is immutable");
    }
    // Persist first; if serialization fails, neither index sees the new state.
    socket.serializeAttachment(state);
    if (
      previous &&
      previous.state.role === state.role &&
      (state.role === "source" || state.role === "target")
    ) {
      previous.state = state;
      return;
    }
    this.remove(socket);
    if (state.role !== "closed") this.add(socket, state);
  }
  private add(socket: WebSocket, state: Attachment): void {
    const connection = { socket, state };
    this.bySocket.set(socket, connection);
    this.byId.set(state.id, connection);
    if (state.role === "control") this.controls.set(state.device, connection);
    if (state.role === "pending") this.pending.set(state.sid, connection);
    if (state.role === "pending" || state.role === "source")
      this.sources.add(socket);
    if (session(state)) {
      const key = `${state.target}/${state.generation}`;
      const sockets = this.bindings.get(key) || new Set<WebSocket>();
      sockets.add(socket);
      this.bindings.set(key, sockets);
    }
  }
  private remove(socket: WebSocket): void {
    const state = this.get(socket)?.state;
    if (!state) return;
    this.bySocket.delete(socket);
    this.byId.delete(state.id);
    this.sources.delete(socket);
    if (state.role === "control") this.controls.delete(state.device);
    if (state.role === "pending") this.pending.delete(state.sid);
    if (session(state)) {
      const key = `${state.target}/${state.generation}`;
      const sockets = this.bindings.get(key);
      sockets?.delete(socket);
      if (!sockets?.size) this.bindings.delete(key);
    }
  }
}
