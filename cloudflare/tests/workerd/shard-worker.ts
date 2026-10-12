import worker, { XrunRelay, XrunData, type Env } from "../../src/index";
export default worker;
export class TestShardRelay extends XrunRelay {
  override async fetch(request: Request): Promise<Response> {
    if (new URL(request.url).pathname === "/test/metrics") {
      return Response.json({
        enumeration: 0,
        alarm: 0,
        rowsWritten: this.ctx.storage.sql
          .exec<{ n: number }>("SELECT total_changes() AS n")
          .one().n,
        changes: this.ctx.storage.sql
          .exec<{ n: number }>("SELECT total_changes() AS n")
          .one().n,
        sockets: this.ctx
          .getWebSockets()
          .map((socket) => socket.deserializeAttachment()),
        slots: this.ctx.storage.sql
          .exec<{ data: string }>("SELECT data FROM relay_shards")
          .toArray()
          .map((row) => JSON.parse(row.data)),
      });
    }
    if (new URL(request.url).pathname === "/test/restore") {
      await this.restoreSockets();
      return Response.json({ restored: true });
    }
    return super.fetch(request);
  }
}
export class TestShardData extends XrunData {
  override async fetch(request: Request): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/test/restore") {
      await this.restore();
      return Response.json({ restored: true });
    }
    if (path === "/test/corrupt") {
      for (const socket of this.ctx.getWebSockets()) {
        const leg = socket.deserializeAttachment() as {
          side: string;
          outstanding: number;
        };
        if (leg.side === "source") {
          leg.outstanding = -1;
          socket.serializeAttachment(leg);
        }
      }
      await this.restore();
      return Response.json({ restored: true });
    }
    if (path === "/test/metrics") {
      const table = this.ctx.storage.sql
        .exec<{ name: string }>(
          "SELECT name FROM sqlite_master WHERE name='data_slot'",
        )
        .toArray();
      const storedRows = table.length
        ? this.ctx.storage.sql
            .exec<{ n: number }>("SELECT COUNT(*) AS n FROM data_slot")
            .one().n
        : 0;
      return Response.json({
        storedRows,
        changes: this.ctx.storage.sql
          .exec<{ n: number }>("SELECT total_changes() AS n")
          .one().n,
        sockets: this.ctx
          .getWebSockets()
          .map((socket) => socket.deserializeAttachment()),
      });
    }
    return super.fetch(request);
  }
}
