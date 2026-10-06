import { error, type Env } from "./relay";
import { route } from "./routes";
export { XrunRelay } from "./relay";
export type { Env } from "./relay";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const parsed = route(url.pathname, env.RELAY_ROUTE);
    if (request.method !== "GET" || url.search || !parsed)
      return new Response("Not found", { status: 404 });
    if (request.headers.get("X-Xrun-Version") !== env.XRUN_VERSION)
      return error("VERSION_MISMATCH", "Relay release differs", 409);
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket")
      return error("INVALID_REQUEST", "WebSocket required", 426);
    const headers = new Headers(request.headers);
    headers.set(
      "X-Xrun-Peer",
      request.headers.get("CF-Connecting-IP") || "unknown",
    );
    return env.NETWORKS.getByName(parsed.network).fetch(
      new Request(request, { headers }),
    );
  },
} satisfies ExportedHandler<Env>;
