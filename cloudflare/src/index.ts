import { error, type Env } from "./relay";
import { route } from "./routes";
import { negotiate, parseRange, PROTOCOL } from "./protocol";
export { XrunRelay } from "./relay";
export type { Env } from "./relay";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const parsed = route(url.pathname, env.RELAY_ROUTE);
    if (request.method !== "GET" || url.search || !parsed)
      return new Response("Not found", { status: 404 });
    const supported = {
      min: env.XRUN_PROTOCOL_MIN,
      max: env.XRUN_PROTOCOL_MAX,
    };
    if (supported.min < PROTOCOL.min || supported.max > PROTOCOL.max)
      return error(
        "VERSION_MISMATCH",
        "Unsupported relay protocol configuration",
        500,
      );
    let selected: number;
    try {
      selected = negotiate(
        supported,
        parseRange(request.headers.get("X-Xrun-Protocol")),
      );
    } catch {
      return error("VERSION_MISMATCH", "No common supported protocol", 409);
    }
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket")
      return error("INVALID_REQUEST", "WebSocket required", 426);
    const headers = new Headers(request.headers);
    headers.set("X-Xrun-Negotiated-Protocol", String(selected));
    headers.set(
      "X-Xrun-Peer",
      request.headers.get("CF-Connecting-IP") || "unknown",
    );
    return env.NETWORKS.getByName(parsed.network).fetch(
      new Request(request, { headers }),
    );
  },
} satisfies ExportedHandler<Env>;
