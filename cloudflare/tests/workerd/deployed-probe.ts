// Temporary acceptance Worker only. Production deploy.ts never bundles this entry.
import worker, { XrunRelay, type Env } from "../../src/index";

export class ProbeRelay extends XrunRelay {
  private instance = crypto.randomUUID();

  override async fetch(request: Request): Promise<Response> {
    if (new URL(request.url).pathname === "/probe") {
      return Response.json({
        instance: this.instance,
        sockets: this.ctx
          .getWebSockets()
          .map((socket) => socket.deserializeAttachment()),
      });
    }
    return super.fetch(request);
  }
}

export default {
  async fetch(
    request: Request,
    env: Env,
    ctx: ExecutionContext,
  ): Promise<Response> {
    const url = new URL(request.url);
    const match = /^\/([^/]+)\/probe\/(net_[a-z2-7]{52})$/.exec(url.pathname);
    if (match && match[1] === env.RELAY_ROUTE && request.method === "GET") {
      return env.NETWORKS.getByName(match[2]).fetch("https://test/probe");
    }
    return worker.fetch(request, env);
  },
} satisfies ExportedHandler<Env>;
