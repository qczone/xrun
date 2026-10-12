import type { DurableObjectLocationHint } from "@cloudflare/workers-types";
/** The coordinator owns global admission; ciphertext belongs to one session object. */
import type { AuthorityState } from "./authority";
import type { TrafficLeg } from "./traffic-state";
import type { DurableObjectStorage } from "@cloudflare/workers-types";
import {
  SESSIONS,
  SESSIONS_PER_SOURCE,
  MANAGER_RESERVED_SESSIONS,
  IDLE,
} from "./limits";
export interface ShardSlot {
  sid: string;
  network: string;
  target: string;
  generation: string;
  source: string | null;
  ip: string;
  management: boolean;
  pairing: boolean;
  authority: AuthorityState;
  sourceTicket: string;
  targetTicket: string;
  releaseTicket: string;
  sourceId: string;
  targetId: string;
  deadline: number;
  cachedSince: number | null;
  ready: boolean;
  warm: boolean;
  closed: boolean;
  closePending: boolean;
  accounted: boolean;
  updated: number;
  region?: DurableObjectLocationHint;
}
export function token(): string {
  return crypto.randomUUID().replaceAll("-", "");
}
export function dataPath(
  slot: ShardSlot,
  side: "source" | "target",
  publicPath = false,
): string {
  const prefix = publicPath
    ? `/.well-known/xrun/data/${slot.network}`
    : `/networks/${slot.network}/data`;
  return `${prefix}/${slot.sid}/${side}/${side === "source" ? slot.sourceTicket : slot.targetTicket}`;
}
export function legs(slot: ShardSlot): TrafficLeg[] {
  return [
    {
      id: slot.sourceId,
      network: slot.network,
      role: "source",
      source: slot.source,
      target: slot.target,
      traffic: [],
    },
    {
      id: slot.targetId,
      network: slot.network,
      role: "target",
      target: slot.target,
      traffic: [],
    },
  ];
}
export class Shards {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS relay_shards(sid TEXT PRIMARY KEY,data TEXT NOT NULL,updated INTEGER NOT NULL) STRICT",
    );
  }
  all(): ShardSlot[] {
    return this.storage.sql
      .exec<{ data: string }>("SELECT data FROM relay_shards")
      .toArray()
      .map((row) => JSON.parse(row.data) as ShardSlot);
  }
  get(sid: string): ShardSlot | undefined {
    const row = this.storage.sql
      .exec<{ data: string }>("SELECT data FROM relay_shards WHERE sid=?", sid)
      .toArray()[0];
    return row ? (JSON.parse(row.data) as ShardSlot) : undefined;
  }
  save(slot: ShardSlot): void {
    slot.updated = Date.now();
    this.storage.sql.exec(
      "INSERT INTO relay_shards VALUES(?,?,?) ON CONFLICT(sid) DO UPDATE SET data=excluded.data,updated=excluded.updated",
      slot.sid,
      JSON.stringify(slot),
      slot.updated,
    );
  }
  ids(): string[] {
    return this.all().flatMap((slot) => [slot.sourceId, slot.targetId]);
  }
  ticket(network: string, sid: string, side: string, ticket: string): boolean {
    const slot = this.get(sid);
    return (
      !!slot &&
      slot.network === network &&
      !slot.closed &&
      slot.deadline > Date.now() &&
      (side === "source"
        ? !slot.warm && slot.sourceTicket === ticket
        : side === "target" && slot.targetTicket === ticket)
    );
  }
  /** Closed receipts last at most one day, bounded even under rapid session churn. */
  prune(): void {
    const closed = this.all()
      .filter((slot) => slot.closed)
      .sort((a, b) => a.updated - b.updated);
    for (const [index, slot] of closed.entries()) {
      if (
        slot.updated > Date.now() - 86_400_000 &&
        index >= closed.length - 4096
      )
        continue;
      this.storage.transactionSync(() => {
        if (!slot.accounted && !slot.warm)
          this.storage.sql.exec(
            "UPDATE traffic_metadata SET incomplete=1 WHERE id=1",
          );
        this.storage.sql.exec(
          "DELETE FROM traffic_checkpoints WHERE id IN (?,?)",
          slot.sourceId,
          slot.targetId,
        );
        this.storage.sql.exec("DELETE FROM relay_shards WHERE sid=?", slot.sid);
      });
    }
  }
  create(
    network: string,
    target: string,
    generation: string,
    source: string | null,
    ip: string,
    management: boolean,
    pairing: boolean,
    authority: AuthorityState,
    warm = false,
  ): ShardSlot {
    return {
      sid: token(),
      network,
      target,
      generation,
      source,
      ip,
      management,
      pairing,
      authority,
      sourceTicket: token(),
      targetTicket: token(),
      releaseTicket: token(),
      sourceId: crypto.randomUUID(),
      targetId: crypto.randomUUID(),
      deadline: Date.now() + IDLE,
      cachedSince: null,
      ready: false,
      warm,
      closed: false,
      closePending: false,
      accounted: false,
      updated: Date.now(),
    };
  }
  reserve(slot: ShardSlot): ShardSlot[] {
    this.prune();
    const reclaimed: ShardSlot[] = [];
    while (true) {
      const active = this.all().filter(
        (value) => !value.warm && !value.closed && value.deadline > Date.now(),
      );
      const key = (value: ShardSlot) => value.source ?? `anonymous:${value.ip}`;
      const sourceFull =
        active.filter((value) => key(value) === key(slot)).length >=
        SESSIONS_PER_SOURCE;
      const ordinaryFull =
        !slot.management &&
        active.filter((value) => !value.management).length >=
          SESSIONS - MANAGER_RESERVED_SESSIONS;
      if (!sourceFull && !ordinaryFull && active.length < SESSIONS) break;
      const oldest = active
        .filter(
          (value) =>
            value.cachedSince !== null &&
            (!sourceFull || key(value) === key(slot)) &&
            (!ordinaryFull || !value.management),
        )
        .sort((a, b) => (a.cachedSince ?? 0) - (b.cachedSince ?? 0))[0];
      if (!oldest) throw new Error("SESSION_LIMIT");
      oldest.closed = true;
      oldest.closePending = true;
      this.save(oldest);
      reclaimed.push(oldest);
    }
    this.save(slot);
    return reclaimed;
  }
}
