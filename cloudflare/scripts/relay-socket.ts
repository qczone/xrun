import { PROTOCOL_HEADER } from "../src/protocol";
/** Explicit network client used by deployed acceptance, with no application heartbeat. */
export class RelaySocket {
  readonly socket: WebSocket;
  private queue: (string | ArrayBuffer)[] = [];
  private waiting?: {
    resolve: (value: string | ArrayBuffer) => void;
    reject: (error: Error) => void;
  };
  private closed = false;
  readonly ready: Promise<void>;

  constructor(url: string, version: string) {
    // DOM typings omit the Bun constructor overload for request headers.
    const NetworkWebSocket = WebSocket as unknown as {
      new (url: string, options: Bun.WebSocketOptions): WebSocket;
    };
    this.socket = new NetworkWebSocket(url, {
      headers: {
        "X-Xrun-Version": version,
        "X-Xrun-Protocol": PROTOCOL_HEADER,
      },
    });
    this.socket.binaryType = "arraybuffer";
    this.ready = new Promise((resolve, reject) => {
      this.socket.addEventListener("open", () => resolve(), { once: true });
      this.socket.addEventListener(
        "error",
        () => reject(new Error("WebSocket failed")),
        { once: true },
      );
    });
    this.socket.addEventListener("message", (event) => {
      const value = event.data as string | ArrayBuffer;
      if (this.waiting) {
        const waiting = this.waiting;
        this.waiting = undefined;
        waiting.resolve(value);
      } else this.queue.push(value);
    });
    this.socket.addEventListener("close", () => {
      this.closed = true;
      this.waiting?.reject(new Error("closed"));
      this.waiting = undefined;
    });
  }
  send(value: object | ArrayBuffer): void {
    this.socket.send(
      value instanceof ArrayBuffer ? value : JSON.stringify(value),
    );
  }
  async next(): Promise<string | ArrayBuffer> {
    const value = this.queue.shift();
    if (value !== undefined) return value;
    if (this.closed) throw new Error("closed");
    if (this.waiting) throw new Error("overlapping reads");
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting = undefined;
        reject(new Error("message timeout"));
      }, 15000);
      this.waiting = {
        resolve: (value) => {
          clearTimeout(timer);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timer);
          reject(error);
        },
      };
    });
  }
  async json(): Promise<Record<string, any>> {
    return JSON.parse(String(await this.next()));
  }
  close(): void {
    this.socket.close();
  }
}
