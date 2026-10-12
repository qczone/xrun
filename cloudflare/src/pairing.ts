/** Public locators carry no password; only the authenticated manager allocates them. */
import { AdmissionError } from "./admission";
import type {
  DurableObjectStorage,
  SqlStorageValue,
} from "@cloudflare/workers-types";
export const CODE_ALPHABET = "23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
export function pairingLocator(path: string): string | undefined {
  const match = /^\/\.well-known\/xrun\/pair\/([A-Z2-9]{2})$/.exec(path);
  return match && [...match[1]].every((c) => CODE_ALPHABET.includes(c))
    ? match[1]
    : undefined;
}
export interface PairingRoom extends Record<string, SqlStorageValue> {
  invitation_id: string;
  locator: string;
  expires_at: number;
}
export class Pairings {
  constructor(private readonly storage: DurableObjectStorage) {
    storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS pairing_rooms (invitation_id TEXT PRIMARY KEY,locator TEXT UNIQUE NOT NULL,expires_at INTEGER NOT NULL) STRICT",
    );
    storage.sql.exec(
      "CREATE TABLE IF NOT EXISTS pairing_peers (peer TEXT PRIMARY KEY,expires INTEGER NOT NULL,hits INTEGER NOT NULL) STRICT",
    );
  }
  /** Called only with the Worker-provided CF peer address, never a caller header. */
  throttlePeer(peer: string, now = Date.now()): void {
    this.storage.transactionSync(() => {
      this.storage.sql.exec("DELETE FROM pairing_peers WHERE expires<=?", now);
      const old = this.storage.sql
        .exec<{ expires: number; hits: number }>(
          "SELECT expires,hits FROM pairing_peers WHERE peer=?",
          peer,
        )
        .toArray()[0];
      const count = this.storage.sql
        .exec<{ count: number }>("SELECT COUNT(*) AS count FROM pairing_peers")
        .one().count;
      if ((old?.hits ?? 0) >= 32 || (!old && count >= 4096))
        throw new AdmissionError(
          "PAIRING_BUSY",
          "Public pairing rate exceeded; retry in a minute",
        );
      this.storage.sql.exec(
        "INSERT INTO pairing_peers VALUES(?,?,?) ON CONFLICT(peer) DO UPDATE SET hits=excluded.hits",
        peer,
        old?.expires ?? now + 60_000,
        (old?.hits ?? 0) + 1,
      );
    });
  }
  find(locator: string): PairingRoom | undefined {
    return this.storage.sql
      .exec<PairingRoom>(
        "SELECT invitation_id,locator,expires_at FROM pairing_rooms WHERE locator=? AND expires_at>?",
        locator,
        Date.now(),
      )
      .toArray()[0];
  }
  cancel(id: string): void {
    this.storage.sql.exec(
      "DELETE FROM pairing_rooms WHERE invitation_id=?",
      id,
    );
  }
  register(id: string, locator: string, expires: number): void {
    if (
      !/^[a-f0-9]{32}$/.test(id) ||
      locator.length !== 2 ||
      ![...locator].every((c) => CODE_ALPHABET.includes(c)) ||
      !Number.isSafeInteger(expires) ||
      expires <= Date.now() ||
      expires > Date.now() + 630_000
    )
      throw new AdmissionError("INVALID_REQUEST", "Invalid pairing metadata");
    this.storage.transactionSync(() => {
      this.storage.sql.exec(
        "DELETE FROM pairing_rooms WHERE expires_at<=?",
        Date.now(),
      );
      const current = this.find(locator);
      if (current && current.invitation_id !== id)
        throw new AdmissionError(
          "PAIRING_BUSY",
          "Pairing locator already allocated",
        );
      if (
        !current &&
        this.storage.sql
          .exec<{ count: number }>(
            "SELECT COUNT(*) AS count FROM pairing_rooms",
          )
          .one().count >= 256
      )
        throw new AdmissionError(
          "INVITATION_LIMIT",
          "Too many active pairing sessions",
        );
      this.storage.sql.exec(
        "INSERT INTO pairing_rooms VALUES(?,?,?) ON CONFLICT(invitation_id) DO UPDATE SET locator=excluded.locator,expires_at=excluded.expires_at",
        id,
        locator,
        expires,
      );
    });
  }
}
