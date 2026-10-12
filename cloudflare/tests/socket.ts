import type { Miniflare } from "miniflare";
export class Socket {
  private queue: (string | ArrayBuffer)[] = [];
  private waiting?: {
    resolve: (value: string | ArrayBuffer) => void;
    reject: (error: Error) => void;
  };
  private closed = false;
  constructor(
    readonly ws: NonNullable<
      Awaited<ReturnType<Miniflare["dispatchFetch"]>>["webSocket"]
    >,
    readonly automaticAuthority = true,
  ) {
    ws.addEventListener("message", (event) => {
      const value = event.data as string | ArrayBuffer;
      if (this.automaticAuthority && typeof value === "string") {
        const parsed = JSON.parse(value);
        if (parsed.type === "authority") {
          const lease = parsed.state.lease.lease;
          this.send({
            type: "authority_ack",
            authority_generation: lease.authority_generation,
            version: lease.version,
            hash: lease.hash,
          });
          return;
        }
      }
      if (this.waiting) {
        const { resolve } = this.waiting;
        this.waiting = undefined;
        resolve(value);
      } else this.queue.push(value);
    });
    ws.addEventListener("close", () => {
      this.closed = true;
      this.waiting?.reject(new Error("closed"));
      this.waiting = undefined;
    });
    ws.accept();
  }
  send(value: object | ArrayBuffer) {
    this.ws.send(value instanceof ArrayBuffer ? value : JSON.stringify(value));
  }
  async next(): Promise<string | ArrayBuffer> {
    const next = this.queue.shift();
    if (next !== undefined) return next;
    if (this.closed) throw new Error("closed");
    if (this.waiting) throw new Error("overlapping socket reads");
    return new Promise<string | ArrayBuffer>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiting = undefined;
        reject(new Error("timeout"));
      }, 5000);
      timer.unref();
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
  async json(includeAuthority = false) {
    for (;;) {
      const value = JSON.parse((await this.next()) as string);
      if (value.type !== "authority" || includeAuthority) return value;
      const lease = value.state.lease.lease;
      this.send({
        type: "authority_ack",
        authority_generation: lease.authority_generation,
        version: lease.version,
        hash: lease.hash,
      });
    }
  }
  close() {
    this.ws.close();
  }
}
