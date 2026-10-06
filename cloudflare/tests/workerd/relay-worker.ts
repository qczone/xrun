import worker, { XrunRelay } from "../../src/index";

export default worker;

// This module is bundled only by the test runner. The deployed Worker has no
// test routes or clock settings. Real workerd alarms run the inherited handler.
export class TestRelay extends XrunRelay {
  override async alarm(): Promise<void> {
    await super.alarm();
    const runs = await this.ctx.storage.get<number>("test-alarm-runs") || 0;
    await this.ctx.storage.put("test-alarm-runs", runs + 1);
  }

  override async fetch(request: Request): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/test/alarm") {
      const { role, sid } = await request.json<{ role?: string; sid?: string }>();
      const runs = await this.ctx.storage.get<number>("test-alarm-runs") || 0;
      for (const socket of this.ctx.getWebSockets()) {
        const state = socket.deserializeAttachment() as { role: string; sid?: string; deadline?: number };
        if ((role || sid) && (!role || state.role === role) && (!sid || state.sid === sid)) {
          state.deadline = Date.now() - 1;
          socket.serializeAttachment(state);
        }
      }
      await this.ctx.storage.setAlarm(Date.now());
      return Response.json({ runs });
    }
    if (path === "/test/state") {
      return Response.json({
        runs: await this.ctx.storage.get<number>("test-alarm-runs") || 0,
        alarm: await this.ctx.storage.getAlarm(),
      });
    }
    return super.fetch(request);
  }
}
