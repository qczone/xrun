import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { packageRelay } from "../scripts/package";
import { PROTOCOL, PROTOCOL_HEADER } from "../src/protocol";

test("the distributable Worker includes its dependencies and runs without a source checkout", async () => {
  const directory = await mkdtemp(join(tmpdir(), "xrun-relay-package-"));
  let runtime: Miniflare | undefined;
  try {
    await packageRelay(directory);
    const source = await readFile(join(directory, "relay.mjs"), "utf8");
    const manifest = JSON.parse(
      await readFile(join(directory, "manifest.json"), "utf8"),
    );
    expect(manifest.sha256).toBe(
      createHash("sha256").update(source).digest("hex"),
    );
    expect(manifest.protocol).toEqual(PROTOCOL);
    runtime = new Miniflare(
      convertV4MiniflareOptions({
        modules: true,
        script: source,
        compatibilityDate: manifest.compatibility_date,
        durableObjects: {
          NETWORKS: { className: "XrunRelay", useSQLite: true },
          DATA: { className: "XrunData", useSQLite: true },
        },
        bindings: {
          XRUN_PROTOCOL_MIN: PROTOCOL.min,
          XRUN_PROTOCOL_MAX: PROTOCOL.max,
        },
      }),
    );
    await runtime.ready;
    const response = await runtime.dispatchFetch(
      `https://test/networks/probe/status`,
      {
        headers: { Upgrade: "websocket", "X-Xrun-Protocol": PROTOCOL_HEADER },
      },
    );
    expect(response.status).toBe(101);
    const socket = response.webSocket;
    if (!socket) throw new Error("Packaged Worker did not open a connection");
    const challenge = await new Promise<unknown>((resolve, reject) => {
      const timeout = setTimeout(
        () =>
          reject(
            new Error(
              "Packaged Worker did not issue an authentication challenge",
            ),
          ),
        5000,
      );
      socket.addEventListener(
        "message",
        (event) => {
          clearTimeout(timeout);
          resolve(JSON.parse(String(event.data)));
        },
        { once: true },
      );
      socket.accept();
    });
    expect(challenge).toMatchObject({ type: "challenge" });
    socket.close();
  } finally {
    await runtime?.dispose();
    await rm(directory, { recursive: true, force: true });
  }
}, 15000);
