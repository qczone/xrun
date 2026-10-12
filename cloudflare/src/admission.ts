/** A relay belongs to its first proven manager; anonymous connections never claim it. */
import type { DurableObjectStorage } from "@cloudflare/workers-types";
import { DEVICE } from "./auth";

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
    this.owner();
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
      this.storage.sql.exec(
        "INSERT INTO relay_owner VALUES(1,?,?)",
        network,
        device,
      );
      return { network_id: network, manager_id: device };
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
