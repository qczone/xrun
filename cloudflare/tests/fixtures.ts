import "reflect-metadata";
import {
  AsnEcSignatureFormatter,
  BasicConstraintsExtension,
  ExtendedKeyUsage,
  ExtendedKeyUsageExtension,
  KeyUsageFlags,
  KeyUsagesExtension,
  SubjectAlternativeNameExtension,
  X509CertificateGenerator,
} from "@peculiar/x509";
import { base32, type Proof } from "../src/auth";
import {
  hexDigest,
  rosterPayload,
  type AuthorityState,
  type Member,
  type Roster,
} from "../src/authority";
export const VERSION = /version = "([^"]+)"/.exec(
  await Bun.file(new URL("../../Cargo.toml", import.meta.url)).text(),
)![1];
const alg = { name: "ECDSA", namedCurve: "P-256" };
const formatter = new AsnEcSignatureFormatter();
export async function fixture() {
  const rootKeys = await crypto.subtle.generateKey(alg, true, [
    "sign",
    "verify",
  ]);
  const root = await X509CertificateGenerator.createSelfSigned({
    serialNumber: "01",
    name: "CN=xrun network root",
    keys: rootKeys,
    notBefore: new Date(Date.now() - 60_000),
    notAfter: new Date(Date.now() + 86_400_000),
    signingAlgorithm: { name: "ECDSA", hash: "SHA-256" },
    extensions: [
      new BasicConstraintsExtension(true, undefined, true),
      new KeyUsagesExtension(KeyUsageFlags.keyCertSign, true),
    ],
  });
  const network = `net_${base32(new Uint8Array(await root.publicKey.getThumbprint("SHA-256")))}`;
  const members: Member[] = [];
  let managerId: string | null = null;
  let authorityGeneration = 1;
  let version = 1;
  let sequence = 0;
  async function sign(
    key: CryptoKey,
    domain: string,
    binding: object,
  ): Promise<string> {
    const raw = await crypto.subtle.sign(
      { name: "ECDSA", hash: "SHA-256" },
      key,
      new TextEncoder().encode(
        `xrun/sig-v2/${domain}\0${JSON.stringify(binding)}`,
      ),
    );
    const der = formatter.toAsnSignature(alg, raw);
    if (!der) throw new Error("Fixture signature encoding failed");
    return Buffer.from(der).toString("base64");
  }
  async function authority(lifetime = 900_000): Promise<AuthorityState> {
    const roster: Roster = {
      network_id: network,
      version,
      manager_id: managerId ?? members[0].device_id,
      authority_generation: authorityGeneration,
      members: members.map((m) => ({ ...m })),
      relay_addresses: ["https://relay.example.com"],
      relay_ca_pem: "",
    };
    const signed = {
      roster,
      ca_pem: root.toString("pem"),
      signature: await sign(rootKeys.privateKey, "roster", [
        roster,
        await hexDigest(root.rawData),
      ]),
    };
    const issued_at_ms = Date.now();
    const lease = {
      network_id: network,
      authority_generation: authorityGeneration,
      version,
      hash: await hexDigest(await rosterPayload(signed)),
      issued_at_ms,
      expires_at_ms: issued_at_ms + lifetime,
    };
    return {
      roster: signed,
      lease: {
        lease,
        signature: await sign(rootKeys.privateKey, "roster-lease", lease),
      },
    };
  }
  function revoke(device: string) {
    const member = members.find((m) => m.device_id === device);
    if (!member) throw new Error("Unknown fixture member");
    member.revoked = true;
    version++;
  }
  function migrate(device: string) {
    managerId = device;
    authorityGeneration++;
    version++;
  }
  async function member(
    options: { expired?: boolean; san?: string; keyUsage?: number } = {},
  ) {
    const name = `member${sequence++}`;
    const keys = await crypto.subtle.generateKey(alg, true, ["sign", "verify"]);
    const device = `dev_${crypto.randomUUID().replaceAll("-", "")}`;
    const cert = await X509CertificateGenerator.create({
      serialNumber: "02",
      subject: `CN=${device}`,
      issuer: root.subject,
      publicKey: keys.publicKey,
      signingKey: rootKeys.privateKey,
      notBefore: new Date(Date.now() - 60_000),
      notAfter: new Date(Date.now() + (options.expired ? -1000 : 86_400_000)),
      signingAlgorithm: { name: "ECDSA", hash: "SHA-256" },
      extensions: [
        new KeyUsagesExtension(
          options.keyUsage ?? KeyUsageFlags.digitalSignature,
          true,
        ),
        new ExtendedKeyUsageExtension([
          ExtendedKeyUsage.serverAuth,
          ExtendedKeyUsage.clientAuth,
        ]),
        new SubjectAlternativeNameExtension([
          { type: "dns", value: options.san ?? `d${device.slice(4)}.xrun` },
        ]),
      ],
    });
    members.push({
      device_id: device,
      name,
      key_fp: await hexDigest(cert.publicKey.rawData),
      revoked: false,
    });
    version++;
    async function proof(
      path: string,
      nonce: string,
      manager = false,
    ): Promise<Proof> {
      if (manager && !managerId) managerId = device;
      const binding = {
        network,
        device,
        path,
        nonce,
        authority_generation: authorityGeneration,
      };
      return {
        device_id: device,
        cert_pem: cert.toString("pem"),
        root_pem: root.toString("pem"),
        signature: await sign(keys.privateKey, "relay-proof", binding),
        manager_signature: manager
          ? await sign(rootKeys.privateKey, "relay-manager", binding)
          : null,
        authority_generation: authorityGeneration,
        authority: await authority(),
      };
    }
    return { device, proof };
  }
  return { network, member, authority, revoke, migrate };
}
