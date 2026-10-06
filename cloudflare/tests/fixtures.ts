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
  async function member(
    options: { expired?: boolean; san?: string; keyUsage?: number } = {},
  ) {
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
    async function sign(
      key: CryptoKey,
      domain: string,
      binding: object,
    ): Promise<string> {
      const raw = await crypto.subtle.sign(
        { name: "ECDSA", hash: "SHA-256" },
        key,
        new TextEncoder().encode(
          `xrun/sig-v1/${domain}\0${JSON.stringify(binding)}`,
        ),
      );
      return Buffer.from(formatter.toAsnSignature(alg, raw)!).toString(
        "base64",
      );
    }
    async function proof(
      path: string,
      nonce: string,
      manager = false,
    ): Promise<Proof> {
      const binding = { network, device, path, nonce };
      return {
        device_id: device,
        cert_pem: cert.toString("pem"),
        root_pem: root.toString("pem"),
        signature: await sign(keys.privateKey, "relay-proof", binding),
        manager_signature: manager
          ? await sign(rootKeys.privateKey, "relay-manager", binding)
          : null,
      };
    }
    return { device, proof };
  }
  return { network, member };
}
