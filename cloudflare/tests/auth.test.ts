import { expect, test } from "bun:test";
import { verifyProof } from "../src/auth";
import { fixture, VERSION } from "./fixtures";

test("member and manager proofs bind network, device, path and fresh challenge", async () => {
  const f = await fixture();
  const member = await f.member();
  const other = await f.member();
  const path = `/networks/${f.network}/control`;
  const proof = await member.proof(path, "nonce", true);
  expect(await verifyProof(proof, f.network, path, "nonce", VERSION)).toEqual({ device: member.device, manager: true });
  for (const [candidate, network, requestPath, nonce] of [
    [{ ...proof, device_id: other.device }, f.network, path, "nonce"],
    [{ ...proof, signature: (await other.proof(path, "nonce")).signature }, f.network, path, "nonce"],
    [proof, f.network, path, "replayed"],
    [proof, f.network, path.replace("control", "status"), "nonce"],
    [proof, (await fixture()).network, path, "nonce"],
    [{ ...proof, manager_signature: proof.signature }, f.network, path, "nonce"],
    [{ ...proof, extra: true }, f.network, path, "nonce"],
  ] as const) {
    await expect(verifyProof(candidate, network, requestPath, nonce, VERSION)).rejects.toThrow();
  }
});

test("expired certificates, wrong SAN and inappropriate key usage are refused", async () => {
  const f = await fixture();
  const path = `/networks/${f.network}/control`;
  for (const options of [{ expired: true }, { san: "someone.xrun" }, { keyUsage: 4 }]) {
    const member = await f.member(options);
    await expect(verifyProof(await member.proof(path, "nonce"), f.network, path, "nonce", VERSION)).rejects.toThrow();
  }
});
