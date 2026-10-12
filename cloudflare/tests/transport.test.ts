import { expect, test } from "bun:test";
import { Miniflare, convertV4MiniflareOptions } from "miniflare";
import { upgradeResponse } from "./upgrade-response";

test("rejected WebSocket upgrades preserve delayed error bodies", async () => {
  const runtime = new Miniflare(
    convertV4MiniflareOptions({
      modules: true,
      compatibilityDate: "2026-10-03",
      script: `export default {
        fetch() {
          const body = new ReadableStream({
            start(controller) {
              setTimeout(() => {
                controller.enqueue(new TextEncoder().encode(
                  JSON.stringify({ type: "error", code: "RELAY_CLAIMED" })
                ));
                controller.close();
              }, 20);
            }
          });
          return new Response(body, {
            status: 403, headers: { "Content-Type": "application/json" }
          });
        }
      };`,
    }),
  );
  try {
    const origin = (await runtime.ready).origin;
    const response = await upgradeResponse(origin);
    expect(response.status).toBe(403);
    expect(await response.json()).toEqual({
      type: "error",
      code: "RELAY_CLAIMED",
    });
  } finally {
    await runtime.dispose();
  }
});
