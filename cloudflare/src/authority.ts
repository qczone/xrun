/** Root-signed membership and time-bounded authorization; relays cannot renew it. */
import { X509Certificate } from "@peculiar/x509";
import { base32, DEVICE, object, signature } from "./auth";
import { SIGNATURE_FORMAT } from "./protocol";

export interface Member {
  device_id: string;
  name: string;
  key_fp: string;
  revoked: boolean;
}
export interface Roster {
  network_id: string;
  version: number;
  manager_id: string;
  authority_generation: number;
  members: Member[];
  relay_addresses: string[];
  relay_ca_pem: string;
}
export interface SignedRoster {
  roster: Roster;
  ca_pem: string;
  signature: string;
}
export interface Freshness {
  network_id: string;
  authority_generation: number;
  version: number;
  hash: string;
  issued_at_ms: number;
  expires_at_ms: number;
}
export interface AuthorityState {
  roster: SignedRoster;
  lease: { lease: Freshness; signature: string };
}
export const AUTHORITY_BYTES = 128 * 1024;
export function integer(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}
export async function hexDigest(bytes: BufferSource): Promise<string> {
  const hash = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from(hash, (byte) => byte.toString(16).padStart(2, "0")).join(
    "",
  );
}
export async function rosterPayload(
  signed: SignedRoster,
): Promise<Uint8Array<ArrayBuffer>> {
  const root = new X509Certificate(signed.ca_pem);
  return new Uint8Array(
    new TextEncoder().encode(
      `xrun/sig-v${SIGNATURE_FORMAT}/roster\0${JSON.stringify([signed.roster, await hexDigest(root.rawData)])}`,
    ),
  );
}
export function requireFresh(state: AuthorityState, now = Date.now()): void {
  const lease = state.lease.lease;
  if (lease.issued_at_ms > now + 30_000 || lease.expires_at_ms <= now)
    throw new Error(
      "ROSTER_EXPIRED: contact the manager to renew membership freshness",
    );
}
export async function verifyAuthority(
  value: unknown,
  network: string,
): Promise<AuthorityState> {
  if (
    !object(value, ["roster", "lease"]) ||
    !object(value.roster, ["roster", "ca_pem", "signature"]) ||
    typeof value.roster.ca_pem !== "string" ||
    value.roster.ca_pem.length > 8192 ||
    typeof value.roster.signature !== "string" ||
    !object(value.roster.roster, [
      "network_id",
      "version",
      "manager_id",
      "authority_generation",
      "members",
      "relay_addresses",
      "relay_ca_pem",
    ]) ||
    !object(value.lease, ["lease", "signature"]) ||
    typeof value.lease.signature !== "string" ||
    !object(value.lease.lease, [
      "network_id",
      "authority_generation",
      "version",
      "hash",
      "issued_at_ms",
      "expires_at_ms",
    ])
  )
    throw new Error("Invalid authority fields");
  const r = value.roster.roster;
  if (
    r.network_id !== network ||
    !integer(r.version) ||
    !integer(r.authority_generation) ||
    typeof r.manager_id !== "string" ||
    !DEVICE.test(r.manager_id) ||
    !Array.isArray(r.members) ||
    r.members.length < 1 ||
    r.members.length > 256 ||
    !Array.isArray(r.relay_addresses) ||
    r.relay_addresses.length < 1 ||
    r.relay_addresses.length > 8 ||
    typeof r.relay_ca_pem !== "string" ||
    r.relay_ca_pem.length > 8192
  )
    throw new Error("Invalid authority roster");
  const ids = new Set<string>(),
    keys = new Set<string>(),
    names = new Set<string>();
  const members: Member[] = r.members.map((m: unknown) => {
    if (
      !object(m, ["device_id", "name", "key_fp", "revoked"]) ||
      typeof m.device_id !== "string" ||
      !DEVICE.test(m.device_id) ||
      typeof m.name !== "string" ||
      !/^[a-z][a-z0-9-]{0,31}$/.test(m.name) ||
      DEVICE.test(m.name) ||
      typeof m.key_fp !== "string" ||
      !/^[a-f0-9]{64}$/.test(m.key_fp) ||
      typeof m.revoked !== "boolean" ||
      ids.has(m.device_id) ||
      keys.has(m.key_fp) ||
      (!m.revoked && names.has(m.name))
    )
      throw new Error("Invalid authority member");
    ids.add(m.device_id);
    keys.add(m.key_fp);
    if (!m.revoked) names.add(m.name);
    return {
      device_id: m.device_id,
      name: m.name,
      key_fp: m.key_fp,
      revoked: m.revoked,
    };
  });
  if (!members.some((m) => m.device_id === r.manager_id && !m.revoked))
    throw new Error("Manager is not an active member");
  const relay_addresses = r.relay_addresses.map((address: unknown) => {
    if (typeof address !== "string") throw new Error("Invalid relay address");
    const url = new URL(address);
    if (
      url.protocol !== "https:" ||
      !url.hostname ||
      url.username ||
      url.password ||
      url.search ||
      url.hash ||
      !(url.pathname === "/" || /^\/[a-z2-7]{26}$/.test(url.pathname))
    )
      throw new Error("Invalid relay address");
    return address;
  });
  const roster: Roster = {
    network_id: network,
    version: r.version,
    manager_id: r.manager_id,
    authority_generation: r.authority_generation,
    members,
    relay_addresses,
    relay_ca_pem: r.relay_ca_pem,
  };
  const root = new X509Certificate(value.roster.ca_pem);
  if (
    `net_${base32(new Uint8Array(await root.publicKey.getThumbprint("SHA-256")))}` !==
    network
  )
    throw new Error("Authority belongs to another root");
  const signed: SignedRoster = {
    roster,
    ca_pem: value.roster.ca_pem,
    signature: value.roster.signature,
  };
  if (
    !(await signature(
      root,
      "roster",
      [roster, await hexDigest(root.rawData)],
      signed.signature,
    ))
  )
    throw new Error("Invalid authority signature");
  const l = value.lease.lease;
  const hash = await hexDigest(await rosterPayload(signed));
  if (
    l.network_id !== network ||
    l.authority_generation !== roster.authority_generation ||
    l.version !== roster.version ||
    l.hash !== hash ||
    !integer(l.issued_at_ms) ||
    !integer(l.expires_at_ms) ||
    l.expires_at_ms <= l.issued_at_ms ||
    l.expires_at_ms - l.issued_at_ms > 86_400_000
  )
    throw new Error("Invalid freshness binding");
  const lease: Freshness = {
    network_id: network,
    authority_generation: roster.authority_generation,
    version: roster.version,
    hash,
    issued_at_ms: l.issued_at_ms,
    expires_at_ms: l.expires_at_ms,
  };
  if (!(await signature(root, "roster-lease", lease, value.lease.signature)))
    throw new Error("Invalid freshness signature");
  return { roster: signed, lease: { lease, signature: value.lease.signature } };
}

/** Both signatures were verified before this synchronous monotonic check. */
export function successor(
  previous: AuthorityState,
  next: AuthorityState,
): void {
  const a = previous.roster.roster,
    b = next.roster.roster;
  if (
    b.network_id !== a.network_id ||
    previous.roster.ca_pem !== next.roster.ca_pem ||
    b.authority_generation < a.authority_generation ||
    b.version < a.version ||
    (b.manager_id !== a.manager_id &&
      b.authority_generation <= a.authority_generation) ||
    (b.authority_generation > a.authority_generation &&
      b.version <= a.version) ||
    (b.version === a.version &&
      next.lease.lease.hash !== previous.lease.lease.hash)
  )
    throw new Error(
      "ROSTER_ROLLBACK: refusing a stale or conflicting authority",
    );
  for (const old of a.members) {
    const member = b.members.find((m) => m.device_id === old.device_id);
    if (
      !member ||
      member.name !== old.name ||
      member.key_fp !== old.key_fp ||
      (old.revoked && !member.revoked)
    )
      throw new Error("Invalid membership successor");
  }
}
