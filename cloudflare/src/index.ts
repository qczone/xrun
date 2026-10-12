import { edgeRegion, region } from "./regions";
import { error, type Env } from "./relay";
import { RELAY_OBJECT, route } from "./routes";
import { negotiate, parseRange, PROTOCOL } from "./protocol";
import { pairingLocator } from "./pairing";
export { XrunRelay } from "./relay";
export { XrunData } from "./data";
export type { Env } from "./relay";

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    const parsed = route(url.pathname, env.RELAY_ROUTE);
    const pairing = pairingLocator(url.pathname);
    const publicData =
      /^\/\.well-known\/xrun\/data\/(net_[a-z2-7]{52})\/([a-f0-9]{32})\/(source|target)\/([a-f0-9]{32})$/.exec(
        url.pathname,
      );
    const dataRoute =
      url.pathname.startsWith(`/${env.RELAY_ROUTE}/`) &&
      /^\/networks\/net_[a-z2-7]{52}\/data\/[a-f0-9]{32}\/(source|target)\/[a-f0-9]{32}$/.test(
        url.pathname.slice(env.RELAY_ROUTE.length + 1),
      );
    if (
      request.method !== "GET" ||
      url.search ||
      (!parsed && !pairing && !dataRoute && !publicData)
    )
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
    if (!env.DATA)
      return error("INVALID_CONFIG", "Data namespace is not configured", 503);
    const headers = new Headers(request.headers);
    headers.set("X-Xrun-Negotiated-Protocol", String(selected));
    headers.set(
      "X-Xrun-Peer",
      request.headers.get("CF-Connecting-IP") || "unknown",
    );
    headers.delete("X-Xrun-Region");
    const hint =
      region(env.XRUN_RELAY_REGION) ?? edgeRegion(request.cf?.continent);
    if (hint) headers.set("X-Xrun-Region", hint);
    const data =
      publicData ??
      /^\/networks\/(net_[a-z2-7]{52})\/data\/([a-f0-9]{32})\/(source|target)\/([a-f0-9]{32})$/.exec(
        url.pathname.slice(env.RELAY_ROUTE.length + 1),
      );
    if (data) {
      if (
        !env.DATA ||
        !(await env.NETWORKS.getByName(RELAY_OBJECT).dataTicket(
          data[1],
          data[2],
          data[3],
          data[4],
        ))
      )
        return error("INVALID_SESSION", "Data ticket unavailable", 403);
      return env.DATA.getByName(
        data[2],
        env.XRUN_RELAY_REGION
          ? { locationHint: env.XRUN_RELAY_REGION }
          : undefined,
      ).fetch(new Request(request, { headers }));
    }
    return env.NETWORKS.getByName(
      RELAY_OBJECT,
      env.XRUN_RELAY_REGION
        ? { locationHint: env.XRUN_RELAY_REGION }
        : undefined,
    ).fetch(new Request(request, { headers }));
  },
} satisfies ExportedHandler<Env>;
