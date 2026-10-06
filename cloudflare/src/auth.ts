import "reflect-metadata";
import { SIGNATURE_FORMAT } from "./protocol";
import {
  AsnEcSignatureFormatter,
  BasicConstraintsExtension,
  ExtendedKeyUsage,
  ExtendedKeyUsageExtension,
  KeyUsageFlags,
  KeyUsagesExtension,
  SubjectAlternativeNameExtension,
  X509Certificate,
} from "@peculiar/x509";

export interface Proof {
  device_id: string;
  cert_pem: string;
  root_pem: string;
  signature: string;
  manager_signature: string | null;
}
export const DEVICE = /^dev_[a-f0-9]{32}$/;
const MAX_CERTIFICATE_PEM_BYTES = 8192;
const MAX_ENCODED_SIGNATURE_CHARS = 128;
const knownExtensions = new Set([
  "2.5.29.14",
  "2.5.29.15",
  "2.5.29.17",
  "2.5.29.19",
  "2.5.29.35",
  "2.5.29.37",
]);
const algorithm = { name: "ECDSA", namedCurve: "P-256" };
const formatter = new AsnEcSignatureFormatter();

export function base32(bytes: Uint8Array): string {
  const alphabet = "abcdefghijklmnopqrstuvwxyz234567";
  let accumulator = 0,
    bitCount = 0,
    output = "";
  for (const byte of bytes) {
    accumulator = (accumulator << 8) | byte;
    bitCount += 8;
    while (bitCount >= 5) {
      bitCount -= 5;
      output += alphabet[(accumulator >>> bitCount) & 31];
    }
  }
  if (bitCount) output += alphabet[(accumulator << (5 - bitCount)) & 31];
  return output;
}
export function randomRoute(): string {
  return base32(crypto.getRandomValues(new Uint8Array(16)));
}
export function object(
  value: unknown,
  keys?: string[],
): value is Record<string, unknown> {
  return (
    !!value &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    (!keys || Object.keys(value).every((key) => keys.includes(key)))
  );
}
async function signature(
  cert: X509Certificate,
  domain: string,
  binding: object,
  encoded: string,
): Promise<boolean> {
  if (encoded.length > MAX_ENCODED_SIGNATURE_CHARS) return false;
  const der = Uint8Array.from(atob(encoded), (character) =>
    character.charCodeAt(0),
  );
  const raw = formatter.toWebSignature(algorithm, der);
  if (!raw || raw.byteLength !== 64) return false;
  const key = await cert.publicKey.export(algorithm, ["verify"]);
  return crypto.subtle.verify(
    { name: "ECDSA", hash: "SHA-256" },
    key,
    raw,
    new TextEncoder().encode(
      `xrun/sig-v${SIGNATURE_FORMAT}/${domain}\0${JSON.stringify(binding)}`,
    ),
  );
}

/** Validates the routing identity only. Rosters and revocation stay at peers. */
export async function verifyProof(
  value: unknown,
  network: string,
  path: string,
  nonce: string,
): Promise<{ device: string; manager: boolean }> {
  if (
    !object(value, [
      "device_id",
      "cert_pem",
      "root_pem",
      "signature",
      "manager_signature",
    ]) ||
    typeof value.device_id !== "string" ||
    !DEVICE.test(value.device_id) ||
    typeof value.cert_pem !== "string" ||
    value.cert_pem.length > MAX_CERTIFICATE_PEM_BYTES ||
    typeof value.root_pem !== "string" ||
    value.root_pem.length > MAX_CERTIFICATE_PEM_BYTES ||
    typeof value.signature !== "string" ||
    (value.manager_signature != null &&
      typeof value.manager_signature !== "string")
  ) {
    throw new Error("Invalid member proof");
  }
  const device = value.device_id;
  const root = new X509Certificate(value.root_pem);
  const member = new X509Certificate(value.cert_pem);
  const fingerprint = base32(
    new Uint8Array(await root.publicKey.getThumbprint("SHA-256")),
  );
  if (network !== `net_${fingerprint}`)
    throw new Error("Proof belongs to another network");
  const now = Date.now();
  for (const cert of [root, member]) {
    if (
      cert.notBefore.getTime() > now ||
      cert.notAfter.getTime() <= now ||
      cert.extensions.some(
        (ext) => ext.critical && !knownExtensions.has(ext.type),
      )
    ) {
      throw new Error("Invalid certificate validity or extensions");
    }
  }
  if (
    !root.getExtension(BasicConstraintsExtension)?.ca ||
    !(
      (root.getExtension(KeyUsagesExtension)?.usages ?? 0) &
      KeyUsageFlags.keyCertSign
    ) ||
    member.getExtension(BasicConstraintsExtension)?.ca ||
    !(
      (member.getExtension(KeyUsagesExtension)?.usages ?? 0) &
      KeyUsageFlags.digitalSignature
    ) ||
    !member
      .getExtension(ExtendedKeyUsageExtension)
      ?.usages.includes(ExtendedKeyUsage.serverAuth) ||
    !member
      .getExtension(SubjectAlternativeNameExtension)
      ?.names.items.some(
        (name) =>
          name.type === "dns" && name.value === `d${device.slice(4)}.xrun`,
      ) ||
    member.subjectName.getField("CN").length !== 1 ||
    member.subjectName.getField("CN")[0] !== value.device_id ||
    member.issuer !== root.subject ||
    !(await member.verify({ publicKey: root.publicKey }))
  ) {
    throw new Error("Invalid member certificate");
  }
  const binding = { network, device: value.device_id, path, nonce };
  if (!(await signature(member, "relay-proof", binding, value.signature))) {
    throw new Error("Invalid member signature");
  }
  const managerSignature = value.manager_signature;
  const manager = typeof managerSignature === "string";
  if (
    manager &&
    !(await signature(root, "relay-manager", binding, managerSignature))
  ) {
    throw new Error("Invalid manager signature");
  }
  return { device: value.device_id, manager };
}
