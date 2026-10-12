import { randomBytes } from "node:crypto";

// Read rejected upgrades as HTTP: Bun's ws adapter can truncate a body that
// arrives after the handshake headers. The successful socket tests use Miniflare.
export function upgradeResponse(
  url: string,
  headers: HeadersInit = {},
): Promise<Response> {
  const requestHeaders = new Headers(headers);
  requestHeaders.set("Upgrade", "websocket");
  requestHeaders.set("Connection", "Upgrade");
  requestHeaders.set("Sec-WebSocket-Version", "13");
  requestHeaders.set("Sec-WebSocket-Key", randomBytes(16).toString("base64"));
  return fetch(url, {
    headers: requestHeaders,
    signal: AbortSignal.timeout(5000),
  });
}
