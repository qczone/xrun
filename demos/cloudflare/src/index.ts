import { DurableObject } from "cloudflare:workers";

interface Env {
  ROOMS: DurableObjectNamespace<RelayRoom>;
  DEMO_TOKEN: string;
}

type Side = "left" | "right";
interface Attachment {
  id: string;
  side: Side;
  peerId?: string;
  bytes: number;
  frames: number;
  plaintextHits: number;
}

// This is a transport experiment. Production membership authentication is not
// implemented here; a temporary bearer secret restricts the test endpoint.
const MAX_FRAME = 64 * 1024;
const MAX_BYTES = 8 * 1024 * 1024;
const marker = new TextEncoder().encode("xrun-cf-demo-plaintext-marker");

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/health" && request.method === "GET") {
      return Response.json({ service: "xrun-cloudflare-demo" });
    }
    if (!env.DEMO_TOKEN || request.headers.get("Authorization") !== `Bearer ${env.DEMO_TOKEN}`) {
      return new Response("Unauthorized", { status: 401 });
    }
    const route = /^\/rooms\/([a-f0-9-]{36})\/(left|right|stats)$/.exec(url.pathname);
    if (request.method !== "GET" || !route || url.search) {
      return new Response("Not found", { status: 404 });
    }
    return env.ROOMS.getByName(route[1]).fetch(request);
  },
} satisfies ExportedHandler<Env>;

export class RelayRoom extends DurableObject<Env> {
  private readonly bootId = crypto.randomUUID();

  private sockets(side?: Side): WebSocket[] {
    return this.ctx.getWebSockets(side).filter((ws) => ws.readyState === WebSocket.OPEN);
  }

  async fetch(request: Request): Promise<Response> {
    const action = new URL(request.url).pathname.split("/").at(-1);
    if (action === "stats") {
      return Response.json({
        bootId: this.bootId,
        sockets: this.sockets().map((ws) => ws.deserializeAttachment() as Attachment),
      });
    }
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("WebSocket required", { status: 426 });
    }
    const side = action as Side;
    if (this.sockets(side).length) {
      return new Response("Side already connected", { status: 409 });
    }
    const [client, server] = Object.values(new WebSocketPair());
    const state: Attachment = {
      id: crypto.randomUUID(), side, bytes: 0, frames: 0, plaintextHits: 0,
    };
    this.ctx.acceptWebSocket(server, [side]);
    server.serializeAttachment(state);
    const other = this.sockets(side === "left" ? "right" : "left")[0];
    if (other) {
      const peer = other.deserializeAttachment() as Attachment;
      peer.peerId = state.id;
      state.peerId = peer.id;
      other.serializeAttachment(peer);
      server.serializeAttachment(state);
      other.send(JSON.stringify({ type: "ready" }));
      server.send(JSON.stringify({ type: "ready" }));
    } else {
      server.send(JSON.stringify({ type: "waiting" }));
    }
    return new Response(null, { status: 101, webSocket: client });
  }

  webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): void {
    const state = ws.deserializeAttachment() as Attachment;
    const peer = this.peer(state);
    if (typeof message === "string" || !peer) {
      this.close(ws, peer, 1008, "Paired binary transport required");
      return;
    }
    if (message.byteLength > MAX_FRAME || state.bytes + message.byteLength > MAX_BYTES) {
      this.close(ws, peer, 1009, "Demo transfer limit exceeded");
      return;
    }
    const bytes = new Uint8Array(message);
    for (let offset = 0; offset <= bytes.length - marker.length; offset++) {
      if (marker.every((byte, index) => bytes[offset + index] === byte)) {
        state.plaintextHits++;
        break;
      }
    }
    state.bytes += bytes.length;
    state.frames++;
    ws.serializeAttachment(state);
    // send() has no awaitable backpressure. The demo caps each direction at
    // 8 MiB; a production relay needs a bounded flow-control protocol instead.
    try {
      peer.send(message);
    } catch {
      this.close(ws, peer, 1011, "Peer write failed");
    }
  }

  private peer(state: Attachment): WebSocket | undefined {
    return this.sockets(state.side === "left" ? "right" : "left").find(
      (ws) => (ws.deserializeAttachment() as Attachment).id === state.peerId,
    );
  }

  private close(ws: WebSocket, peer: WebSocket | undefined, code: number, reason: string): void {
    ws.close(code, reason);
    peer?.close(code, reason);
  }

  webSocketClose(ws: WebSocket): void {
    const state = ws.deserializeAttachment() as Attachment;
    this.peer(state)?.close(1000, "Peer disconnected");
  }

  webSocketError(ws: WebSocket): void {
    const state = ws.deserializeAttachment() as Attachment;
    this.close(ws, this.peer(state), 1011, "Transport error");
  }
}
