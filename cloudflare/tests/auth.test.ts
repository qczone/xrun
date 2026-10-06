import { expect, test } from "bun:test";
import { verifyProof } from "../src/auth";
import { fixture } from "./fixtures";
import vectors from "../../tests/fixtures/signatures.json";

test("frozen Rust and Cloudflare proof vectors keep signature bytes independent of releases", async () => {
  const binding = vectors.records["relay-proof"].value;
  const proof = {
    device_id: binding.device,
    cert_pem: vectors.member_pem,
    root_pem: vectors.root_pem,
    signature: vectors.records["relay-proof"].signature,
    manager_signature: vectors.records["relay-manager"].signature,
  };
  for (const domain of ["relay-proof", "relay-manager"] as const)
    expect(vectors.records[domain].payload_utf8).toBe(
      `xrun/sig-v1/${domain}\0${JSON.stringify(binding)}`,
    );
  expect(
    await verifyProof(proof, binding.network, binding.path, binding.nonce),
  ).toEqual({ device: binding.device, manager: true });
  await expect(
    verifyProof(proof, binding.network, binding.path, "replay"),
  ).rejects.toThrow();
});

test("member and manager proofs bind network, device, path and fresh challenge", async () => {
  const f = await fixture();
  const member = await f.member();
  const other = await f.member();
  const path = `/networks/${f.network}/control`;
  const proof = await member.proof(path, "nonce", true);
  expect(await verifyProof(proof, f.network, path, "nonce")).toEqual({
    device: member.device,
    manager: true,
  });
  for (const [candidate, network, requestPath, nonce] of [
    [{ ...proof, device_id: other.device }, f.network, path, "nonce"],
    [
      { ...proof, signature: (await other.proof(path, "nonce")).signature },
      f.network,
      path,
      "nonce",
    ],
    [proof, f.network, path, "replayed"],
    [proof, f.network, path.replace("control", "status"), "nonce"],
    [proof, (await fixture()).network, path, "nonce"],
    [
      { ...proof, manager_signature: proof.signature },
      f.network,
      path,
      "nonce",
    ],
    [{ ...proof, extra: true }, f.network, path, "nonce"],
  ] as const) {
    await expect(
      verifyProof(candidate, network, requestPath, nonce),
    ).rejects.toThrow();
  }
});

test("expired certificates, wrong SAN and inappropriate key usage are refused", async () => {
  const f = await fixture();
  const path = `/networks/${f.network}/control`;
  for (const options of [
    { expired: true },
    { san: "someone.xrun" },
    { keyUsage: 4 },
  ]) {
    const member = await f.member(options);
    await expect(
      verifyProof(await member.proof(path, "nonce"), f.network, path, "nonce"),
    ).rejects.toThrow();
  }
});
