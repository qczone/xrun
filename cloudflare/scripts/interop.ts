import "reflect-metadata";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  BasicConstraintsExtension, ExtendedKeyUsage, ExtendedKeyUsageExtension,
  KeyUsageFlags, KeyUsagesExtension, SubjectAlternativeNameExtension,
  X509CertificateGenerator,
} from "@peculiar/x509";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { base32, randomRoute } from "../src/auth";
import { VERSION } from "../tests/fixtures";

// Only the outer transport CA is a JS fixture. Network authority, member
// certificates, signatures, encrypted sessions and operations use real Rust CLI.
const algorithm = { name: "ECDSA", namedCurve: "P-256" };
const caKeys = await crypto.subtle.generateKey(algorithm, true, ["sign", "verify"]);
const serverKeys = await crypto.subtle.generateKey(algorithm, true, ["sign", "verify"]);
const validity = { notBefore: new Date(Date.now() - 60_000), notAfter: new Date(Date.now() + 86_400_000) };
const signingAlgorithm = { name: "ECDSA", hash: "SHA-256" };
const ca = await X509CertificateGenerator.createSelfSigned({
  serialNumber: "01", name: "CN=xrun workerd test transport CA", keys: caKeys,
  ...validity, signingAlgorithm,
  extensions: [new BasicConstraintsExtension(true, undefined, true), new KeyUsagesExtension(KeyUsageFlags.keyCertSign, true)],
});
const server = await X509CertificateGenerator.create({
  serialNumber: "02", subject: "CN=localhost", issuer: ca.subject,
  publicKey: serverKeys.publicKey, signingKey: caKeys.privateKey,
  ...validity, signingAlgorithm,
  extensions: [
    new KeyUsagesExtension(KeyUsageFlags.digitalSignature, true),
    new ExtendedKeyUsageExtension([ExtendedKeyUsage.serverAuth]),
    new SubjectAlternativeNameExtension([{ type: "ip", value: "127.0.0.1" }, { type: "dns", value: "localhost" }]),
  ],
});
const key = Buffer.from(await crypto.subtle.exportKey("pkcs8", serverKeys.privateKey)).toString("base64");
const pemKey = `-----BEGIN PRIVATE KEY-----\n${key.match(/.{1,64}/g)!.join("\n")}\n-----END PRIVATE KEY-----\n`;
const route = randomRoute();
const directory = await mkdtemp(join(tmpdir(), "xrun-workerd-"));
let runtime: Miniflare | undefined;
let child: ReturnType<typeof Bun.spawn> | undefined;
const stop = () => { child?.kill("SIGTERM"); };
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
try {
  runtime = new Miniflare(convertV4MiniflareOptions({
    host: "127.0.0.1", port: 0, https: true, httpsKey: pemKey,
    httpsCert: `${server.toString("pem")}\n${ca.toString("pem")}`,
    modules: true, scriptPath: resolve(import.meta.dir, "../.wrangler/build/index.js"),
    compatibilityDate: "2026-10-03",
    durableObjects: { NETWORKS: { className: "XrunRelay", useSQLite: true } },
    bindings: { RELAY_ROUTE: route, XRUN_VERSION: VERSION },
  }));
  const address = await runtime.ready;
  const pin = base32(new Uint8Array(await ca.publicKey.getThumbprint("SHA-256")));
  const linkFile = join(directory, "relay-link");
  // Exercise normal production pin, chain, validity and hostname verification.
  await writeFile(linkFile, `xrun-relay://${address.host}/${pin}#${route}\n`, { mode: 0o600 });
  child = Bun.spawn(["cargo", "test", "--locked", "--test", "cloudflare", "--", "--ignored", "--nocapture", "--test-threads=1"], {
    cwd: resolve(import.meta.dir, "../.."),
    env: {
      ...process.env, XRUN_TEST_CF_LINK_FILE: linkFile,
      XRUN_TEST_LOG_DIR: process.env.XRUN_TEST_LOG_DIR || resolve(import.meta.dir, "../../target/test-logs"),
    },
    stdin: "ignore", stdout: "inherit", stderr: "inherit",
  });
  const code = await child.exited;
  if (code !== 0) throw new Error(`Rust/workerd interoperability tests failed (${code})`);
} finally {
  process.off("SIGINT", stop);
  process.off("SIGTERM", stop);
  await runtime?.dispose();
  await rm(directory, { recursive: true, force: true });
}
