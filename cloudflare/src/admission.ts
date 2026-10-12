/** A relay belongs to its first proven manager; anonymous connections never claim it. */
import type { DurableObjectStorage } from "@cloudflare/workers-types";
import { DEVICE, type VerifiedProof } from "./auth";
import {
  requireFresh,
  successor,
  verifyAuthority,
  type AuthorityState,
} from "./authority";

export type RelayOwner = { network_id: string; manager_id: string };
export class AdmissionError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
}
export class Admission {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS relay_owner (
      id INTEGER PRIMARY KEY CHECK(id=1), network_id TEXT NOT NULL, manager_id TEXT NOT NULL
    ) STRICT`);
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS relay_authority (
      id INTEGER PRIMARY KEY CHECK(id=1), data TEXT NOT NULL
    ) STRICT`);
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS authority_ack (
      device TEXT PRIMARY KEY, generation INTEGER NOT NULL, version INTEGER NOT NULL, hash TEXT NOT NULL
    ) STRICT`);
    this.owner();
  }
  state(): AuthorityState | null {
    const row = this.storage.sql
      .exec<{ data: string }>("SELECT data FROM relay_authority WHERE id=1")
      .toArray()[0];
    return row ? (JSON.parse(row.data) as AuthorityState) : null;
  }
  async validate(): Promise<void> {
    const state = this.state();
    if (state) {
      await verifyAuthority(state, state.roster.roster.network_id);
      if (
        this.owner()?.manager_id !== state.roster.roster.manager_id ||
        this.owner()?.network_id !== state.roster.roster.network_id
      )
        throw new Error("Invalid persisted authority owner");
    }
  }
  publish(state: AuthorityState): boolean {
    requireFresh(state);
    return this.storage.transactionSync(() => {
      const owner = this.owner(),
        previous = this.state();
      if (owner && owner.network_id !== state.roster.roster.network_id)
        throw new AdmissionError(
          "RELAY_CLAIMED",
          "Relay belongs to another network",
        );
      if (previous) {
        successor(previous, state);
        if (
          previous.lease.lease.hash === state.lease.lease.hash &&
          previous.lease.lease.issued_at_ms >= state.lease.lease.issued_at_ms
        )
          return false;
      } else if (owner && owner.manager_id !== state.roster.roster.manager_id)
        throw new AdmissionError(
          "NOT_MANAGER",
          "Existing manager must first publish authority",
        );
      this.storage.sql.exec(
        "INSERT INTO relay_owner VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET manager_id=excluded.manager_id",
        state.roster.roster.network_id,
        state.roster.roster.manager_id,
      );
      this.storage.sql.exec(
        "INSERT INTO relay_authority VALUES(1,?) ON CONFLICT(id) DO UPDATE SET data=excluded.data",
        JSON.stringify(state),
      );
      return true;
    });
  }
  authenticate(proof: VerifiedProof): boolean {
    if (!this.owner() && !proof.manager)
      throw new AdmissionError("RELAY_UNCLAIMED", "Create a network first");
    const changed = proof.authority ? this.publish(proof.authority) : false;
    const state = this.state();
    if (!state)
      throw new AdmissionError(
        "ROSTER_EXPIRED",
        "Manager freshness is unavailable",
      );
    requireFresh(state);
    const member = state.roster.roster.members.find(
      (m) => m.device_id === proof.device,
    );
    if (!member || member.revoked)
      throw new AdmissionError("DEVICE_REVOKED", "Member is revoked or absent");
    if (
      member.key_fp !== proof.keyFingerprint ||
      proof.authorityGeneration !== state.roster.roster.authority_generation
    )
      throw new AdmissionError(
        "MEMBERSHIP_CHANGED",
        "Reconnect using current authority",
      );
    return changed;
  }
  ack(device: string, generation: number, version: number, hash: string): void {
    const state = this.state();
    if (!state) throw new Error("Authority is missing");
    const current = state.lease.lease;
    if (
      generation < current.authority_generation ||
      (generation === current.authority_generation && version < current.version)
    )
      return;
    if (
      generation !== current.authority_generation ||
      version !== current.version ||
      hash !== current.hash
    )
      throw new Error("Invalid authority acknowledgement");
    this.storage.sql.exec(
      `INSERT INTO authority_ack VALUES(?,?,?,?) ON CONFLICT(device) DO UPDATE SET
       generation=excluded.generation,version=excluded.version,hash=excluded.hash`,
      device,
      generation,
      version,
      hash,
    );
  }
  pending(): string[] {
    const state = this.state();
    if (!state) return [];
    const current = state.lease.lease;
    const acked = new Set(
      this.storage.sql
        .exec<{ device: string }>(
          "SELECT device FROM authority_ack WHERE generation=? AND version=? AND hash=?",
          current.authority_generation,
          current.version,
          current.hash,
        )
        .toArray()
        .map((r) => r.device),
    );
    return state.roster.roster.members
      .filter((m) => !m.revoked && !acked.has(m.device_id))
      .map((m) => m.device_id);
  }
  owner(): RelayOwner | null {
    const owner = this.storage.sql
      .exec<RelayOwner>(
        "SELECT network_id,manager_id FROM relay_owner WHERE id=1",
      )
      .toArray()[0];
    if (!owner) return null;
    if (
      !/^net_[a-z2-7]{52}$/.test(owner.network_id) ||
      !DEVICE.test(owner.manager_id)
    )
      throw new AdmissionError(
        "INVALID_RELAY",
        "Invalid persisted relay owner",
      );
    return owner;
  }
  check(network: string, claiming: boolean): void {
    const owner = this.owner();
    if (owner && owner.network_id !== network)
      throw new AdmissionError(
        "RELAY_CLAIMED",
        "Relay already belongs to a network; ask its manager for a join invitation",
      );
    if (!owner && !claiming)
      throw new AdmissionError(
        "RELAY_UNCLAIMED",
        "Create a network with xrun up before using this relay",
      );
  }
  claim(network: string, device: string, manager: boolean): RelayOwner {
    if (!manager)
      throw new AdmissionError(
        "NOT_MANAGER",
        "Only a proven network manager can claim a relay",
      );
    return this.storage.transactionSync(() => {
      const owner = this.owner();
      if (owner) {
        if (owner.network_id !== network || owner.manager_id !== device)
          throw new AdmissionError(
            "RELAY_CLAIMED",
            "Relay is already bound to another manager",
          );
        return owner;
      }
      throw new AdmissionError(
        "ROSTER_EXPIRED",
        "Publish signed authority before claiming",
      );
    });
  }
  manager(device: string, proof: boolean): boolean {
    if (proof && this.owner()?.manager_id !== device)
      throw new AdmissionError(
        "NOT_MANAGER",
        "Manager proof differs from the bound relay owner",
      );
    return proof;
  }
}
