import "reflect-metadata";
import {
  AsnEcSignatureFormatter, BasicConstraintsExtension, ExtendedKeyUsage,
  ExtendedKeyUsageExtension, KeyUsageFlags, KeyUsagesExtension,
  SubjectAlternativeNameExtension, X509Certificate,
} from "@peculiar/x509";

export interface Proof {
  device_id: string;
  cert_pem: string;
  root_pem: string;
  signature: string;
  manager_signature: string | null;
}
export const DEVICE = /^dev_[a-f0-9]{32}$/;
const knownExtensions = new Set([
  "2.5.29.14", "2.5.29.15", "2.5.29.17", "2.5.29.19", "2.5.29.35", "2.5.29.37",
]);
const algorithm = { name: "ECDSA", namedCurve: "P-256" };
const formatter = new AsnEcSignatureFormatter();

export function base32(bytes: Uint8Array): string {
  const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
  let acc = 0, bits = 0, out = "";
  for (const byte of bytes) {
    acc = (acc << 8) | byte;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      out += alphabet[(acc >>> bits) & 31];
    }
  }
  if (bits) out += alphabet[(acc << (5 - bits)) & 31];
  return out;
}
export function randomRoute(): string {
  return base32(crypto.getRandomValues(new Uint8Array(16)));
}
export function object(value: unknown, keys: string[]): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value)
    && Object.keys(value).every((key) => keys.includes(key));
}
async function signature(cert: X509Certificate, domain: string, binding: object, encoded: string, version: string): Promise<boolean> {
  if (encoded.length > 128) return false;
  const der = Uint8Array.from(atob(encoded), (c) => c.charCodeAt(0));
  const raw = formatter.toWebSignature(algorithm, der);
  if (!raw || raw.byteLength !== 64) return false;
  const key = await cert.publicKey.export(algorithm, ["verify"]);
  return crypto.subtle.verify(
    { name: "ECDSA", hash: "SHA-256" }, key, raw,
    new TextEncoder().encode(`xrun/${version}/${domain}\0${JSON.stringify(binding)}`),
  );
}

/** Validates the routing identity only. Rosters and revocation stay at peers. */
export async function verifyProof(value: unknown, network: string, path: string, nonce: string, version: string): Promise<{ device: string; manager: boolean }> {
  if (!object(value, ["device_id", "cert_pem", "root_pem", "signature", "manager_signature"])
    || typeof value.device_id !== "string" || !DEVICE.test(value.device_id)
    || typeof value.cert_pem !== "string" || value.cert_pem.length > 8192
    || typeof value.root_pem !== "string" || value.root_pem.length > 8192
    || typeof value.signature !== "string"
    || (value.manager_signature != null && typeof value.manager_signature !== "string")) {
    throw new Error("Invalid member proof");
  }
  const device = value.device_id;
  const root = new X509Certificate(value.root_pem);
  const member = new X509Certificate(value.cert_pem);
  const fingerprint = base32(new Uint8Array(await root.publicKey.getThumbprint("SHA-256")));
  if (network !== `net_${fingerprint}`) throw new Error("Proof belongs to another network");
  const now = Date.now();
  for (const cert of [root, member]) {
    if (cert.notBefore.getTime() > now || cert.notAfter.getTime() <= now
      || cert.extensions.some((ext) => ext.critical && !knownExtensions.has(ext.type))) {
      throw new Error("Invalid certificate validity or extensions");
    }
  }
  if (!root.getExtension(BasicConstraintsExtension)?.ca
    || !(root.getExtension(KeyUsagesExtension)?.usages! & KeyUsageFlags.keyCertSign)
    || member.getExtension(BasicConstraintsExtension)?.ca
    || !(member.getExtension(KeyUsagesExtension)?.usages! & KeyUsageFlags.digitalSignature)
    || !member.getExtension(ExtendedKeyUsageExtension)?.usages.includes(ExtendedKeyUsage.serverAuth)
    || !member.getExtension(SubjectAlternativeNameExtension)?.names.items.some(
      (name) => name.type === "dns" && name.value === `d${device.slice(4)}.xrun`,
    )
    || member.subjectName.getField("CN").length !== 1
    || member.subjectName.getField("CN")[0] !== value.device_id
    || member.issuer !== root.subject
    || !await member.verify({ publicKey: root.publicKey })) {
    throw new Error("Invalid member certificate");
  }
  const binding = { network, device: value.device_id, path, nonce };
  if (!await signature(member, "relay-proof", binding, value.signature, version)) {
    throw new Error("Invalid member signature");
  }
  const manager = value.manager_signature != null;
  if (manager && !await signature(root, "relay-manager", binding, value.manager_signature as string, version)) {
    throw new Error("Invalid manager signature");
  }
  return { device: value.device_id, manager };
}
