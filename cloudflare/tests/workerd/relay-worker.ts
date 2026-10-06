import worker, { XrunRelay, type Env } from "../../src/index";
import type { ChallengeState } from "../../src/state";

export default worker;

interface Reads {
  enumeration: number;
  alarm: number;
}
function observed<Props>(
  ctx: DurableObjectState<Props>,
  reads: Reads,
): DurableObjectState<Props> {
  const storage = new Proxy(ctx.storage, {
    get(target, key) {
      if (key === "getAlarm")
        return (...args: Parameters<DurableObjectStorage["getAlarm"]>) => {
          reads.alarm++;
          return target.getAlarm(...args);
        };
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
  return new Proxy(ctx, {
    get(target, key) {
      if (key === "storage") return storage;
      if (key === "getWebSockets")
        return (...args: Parameters<DurableObjectState["getWebSockets"]>) => {
          reads.enumeration++;
          return target.getWebSockets(...args);
        };
      const value = Reflect.get(target, key, target);
      return typeof value === "function" ? value.bind(target) : value;
    },
  });
}

// This module is bundled only by the test runner. The deployed Worker has no
// test routes or clock settings. Real workerd alarms run the inherited handler.
export class TestRelay extends XrunRelay {
  private reads: Reads;
  private verifying = 0;
  private paused = false;
  private gate: string;
  constructor(
    ctx: DurableObjectState<{}>,
    env: Env & { XRUN_TEST_PROOF_GATE: string },
  ) {
    const reads: Reads = { enumeration: 0, alarm: 0 };
    super(ctx, env);
    this.ctx = observed(ctx, reads);
    this.reads = reads;
    this.gate = env.XRUN_TEST_PROOF_GATE;
  }
  protected override async proof(value: unknown, state: ChallengeState) {
    const proof = await super.proof(value, state);
    if (this.paused) {
      this.verifying++;
      await fetch(`${this.gate}/hold`);
    }
    return proof;
  }
  override async alarm(): Promise<void> {
    await super.alarm();
    const runs = (await this.ctx.storage.get<number>("test-alarm-runs")) || 0;
    await this.ctx.storage.put("test-alarm-runs", runs + 1);
  }

  override async fetch(request: Request): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/test/metrics") return Response.json(this.reads);
    if (path === "/test/pause") {
      this.paused = true;
      return Response.json({ paused: true });
    }
    if (path === "/test/release") {
      this.paused = false;
      await fetch(`${this.gate}/release`);
      return Response.json({ released: true });
    }
    if (path === "/test/restore") {
      const { sid, role } = await request.json<{
        sid?: string;
        role?: string;
      }>();
      if (sid)
        for (const socket of this.ctx.getWebSockets()) {
          const state = socket.deserializeAttachment() as {
            role: string;
            sid?: string;
            outstanding?: number;
          };
          if (state.sid === sid && (!role || state.role === role)) {
            state.outstanding = -1;
            socket.serializeAttachment(state);
          }
        }
      this.restoreSockets();
      return Response.json({ restored: true });
    }
    if (path === "/test/alarm") {
      const { role, sid } = await request.json<{
        role?: string;
        sid?: string;
      }>();
      const runs = (await this.ctx.storage.get<number>("test-alarm-runs")) || 0;
      for (const socket of this.ctx.getWebSockets()) {
        const state = socket.deserializeAttachment() as {
          role: string;
          sid?: string;
          deadline?: number;
        };
        if (
          (role || sid) &&
          (!role || state.role === role) &&
          (!sid || state.sid === sid)
        ) {
          state.deadline = Date.now() - 1;
          socket.serializeAttachment(state);
        }
      }
      this.restoreSockets();
      await this.ctx.storage.setAlarm(Date.now());
      return Response.json({ runs });
    }
    if (path === "/test/state") {
      return Response.json({
        runs: (await this.ctx.storage.get<number>("test-alarm-runs")) || 0,
        alarm: await this.ctx.storage.getAlarm(),
        verifying: this.verifying,
      });
    }
    return super.fetch(request);
  }
}
