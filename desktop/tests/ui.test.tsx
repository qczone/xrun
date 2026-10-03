import { expect, spyOn, test } from "bun:test";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { StrictMode } from "react";
import { App } from "../src/App";
import type { Device, Job, Settings, Status } from "../src/api";

type Args = Record<string, unknown>;
type Handler = (args: Args) => unknown;

async function fixture(handlers: Record<string, Handler> = {}, joined = true) {
  const calls: { command: string; args: Args }[] = [];
  const status: Status = {
    local: {
      joined,
      device_id: "self",
      name: "mac1",
      version: "0.0.1-beta.1",
      daemon_running: true,
      daemon_connected: true,
      remote_access_paused: false,
      allow_all: false,
      daemon_installed: true,
    },
    service: {
      app_at_login: false,
      installed: true,
      legacy_installed: false,
      approval_required: false,
      development: false,
    },
    allow_from: [],
    deny_from: [],
    error: null,
  };
  const settings: Settings = {
    execution: { default_cwd: null, max_concurrent_jobs: 4, path: null },
    home_dir: "/Users/test",
    data_dir: "/Users/test/.xrun",
    os: "macos",
  };
  const devices: Device[] = [
    {
      device_id: "unsafe-label",
      name: '<img id="injected" src=x onerror=alert(1)>',
      online: true,
      revoked: false,
      os: "windows",
    },
    {
      device_id: "revoked",
      name: "old-device",
      online: false,
      revoked: true,
      os: "linux",
    },
  ];
  const timers = new Map<number, () => void>();
  let timerId = 0;
  const realSetInterval = globalThis.setInterval;
  const realClearInterval = globalThis.clearInterval;
  // Control application polling without swallowing waitFor's retry timers.
  spyOn(globalThis, "setInterval").mockImplementation(((
    callback: () => void,
    delay?: number,
  ) => {
    if (delay !== 3000) return realSetInterval(callback, delay);
    timers.set(--timerId, callback);
    return timerId;
  }) as unknown as typeof setInterval);
  spyOn(globalThis, "clearInterval").mockImplementation((id) => {
    if (!timers.delete(Number(id))) realClearInterval(Number(id));
  });
  mockIPC((command, payload) => {
    const args = (payload || {}) as Args;
    calls.push({ command, args });
    if (handlers[command]) return handlers[command](args);
    switch (command) {
      case "status":
        return structuredClone(status);
      case "settings":
        return structuredClone(settings);
      case "devices":
        return { devices, server_error: null };
      case "save_settings":
        settings.execution = args.execution as Settings["execution"];
        return null;
      case "join":
        status.local.joined = true;
        return null;
      case "permission":
        status.allow_from = args.allow ? [args.device as string] : [];
        status.deny_from = args.allow ? [] : [args.device as string];
        return null;
      case "all_permissions":
        status.local.allow_all = args.allow as boolean;
        return null;
      case "pause_access":
        status.local.remote_access_paused = args.paused as boolean;
        return null;
      case "autostart":
        status.service.app_at_login = args.enabled as boolean;
        return null;
      case "task_history":
        return { db_id: null, jobs: [], next_cursor: null };
      case "file_history":
        return { entries: [], next_cursor: null };
      default:
        return null;
    }
  });
  const view = render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
  await waitFor(() =>
    expect(
      screen.getByLabelText("默认工作目录").getAttribute("placeholder"),
    ).toBe("/Users/test"),
  );
  await act(async () => {});
  const poll = async () => {
    await act(async () => {
      for (const callback of [...timers.values()]) callback();
    });
  };
  const page = (name: string) =>
    fireEvent.click(screen.getByRole("button", { name }));
  return { ...view, calls, status, settings, poll, page };
}

function task(id = "ABC123", changes: Partial<Job> = {}): Job {
  return {
    job_id: id,
    db_id: "db-original",
    source_device_id: "unsafe-label",
    target_device_id: "self",
    program: "echo",
    args: ['<img id="command-injected" src=x>'],
    cwd: "/repo",
    state: "running",
    exit_code: null,
    signal: null,
    duration_ms: null,
    last_seq: 3,
    output_complete: true,
    incomplete_reason: null,
    error: null,
    created_at_ms: Date.now() - 1000,
    updated_at_ms: Date.now(),
    leftover_possible: false,
    ...changes,
  };
}
const event = (seq: number, stream: string, bytes: string | number[]) => ({
  seq,
  stream,
  data_base64: Buffer.from(
    typeof bytes === "string" ? bytes : new Uint8Array(bytes),
  ).toString("base64"),
});

test("macOS dev mode allows daemon start but disables App login startup", async () => {
  const { calls, status, poll, page } = await fixture();
  status.service.development = true;
  status.service.installed = false;
  status.local.daemon_running = false;
  status.local.daemon_connected = false;
  await poll();
  page("设置");
  expect(
    (
      screen.getByRole("checkbox", {
        name: /^登录时打开 App/,
      }) as HTMLInputElement
    ).disabled,
  ).toBe(true);
  expect(
    screen.getByText("开发模式手动启动，退出 App 后仍继续运行"),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "启动服务" }));
  await waitFor(() =>
    expect(calls.some((call) => call.command === "start")).toBe(true),
  );
  expect(calls.some((call) => call.command === "autostart")).toBe(false);
});

test("settings save only execution fields and preserve unsaved input across polling and navigation", async () => {
  const { calls, poll, page } = await fixture();
  page("设置");
  const input = screen.getByLabelText("任务并发上限") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "8" } });
  await poll();
  page("本机状态");
  page("设置");
  expect(input.value).toBe("8");
  expect(
    (screen.getByRole("button", { name: "保存更改" }) as HTMLButtonElement)
      .disabled,
  ).toBe(false);
  fireEvent.submit(document.getElementById("execution-form")!);
  await waitFor(() => expect(screen.getByText("执行环境已保存")).toBeTruthy());
  expect(calls.find((call) => call.command === "save_settings")?.args).toEqual({
    execution: { default_cwd: null, max_concurrent_jobs: 8, path: null },
  });
  expect(
    (screen.getByRole("button", { name: "保存更改" }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
});

test("task output deduplicates sequences, decodes split UTF-8 and renders failures as text", async () => {
  let reads = 0;
  const { calls, page, poll } = await fixture({
    task_history: () => ({
      db_id: "db-original",
      jobs: [task()],
      next_cursor: null,
    }),
    task_output: () =>
      ++reads === 1
        ? {
            job: task(),
            has_more: false,
            events: [
              event(1, "stdout", [0xe4, 0xb8]),
              event(2, "stderr", "<img id=log-injected>\n"),
              event(3, "stdout", [0xad]),
            ],
          }
        : {
            job: task("ABC123", {
              state: "exited",
              exit_code: 7,
              duration_ms: 1234,
              last_seq: 4,
              output_complete: false,
              incomplete_reason: "TRUNCATED",
            }),
            has_more: false,
            events: [
              event(3, "stdout", [0xad]),
              event(4, "stderr", "\u001b[31mfailed\u001b[0m\n"),
            ],
          },
  });
  page("任务与日志");
  fireEvent.click(await screen.findByRole("button", { name: /ABC123/ }));
  await waitFor(() =>
    expect(document.getElementById("task-log")?.textContent).toContain("中"),
  );
  expect(document.getElementById("task-log")?.textContent).not.toContain("�");
  expect(document.getElementById("command-injected")).toBeNull();
  expect(document.getElementById("log-injected")).toBeNull();
  await poll();
  expect(
    document.getElementById("task-log")?.textContent?.match(/中/g)?.length,
  ).toBe(1);
  expect(screen.getByText("异常退出")).toBeTruthy();
  expect(screen.getByText("退出码 7")).toBeTruthy();
  expect(document.getElementById("task-warning")?.textContent).toContain(
    "输出已被截断",
  );
  expect(document.getElementById("task-log")?.textContent).not.toContain(
    "\u001b",
  );
  expect(
    calls.filter((call) => call.command === "task_output")[1].args,
  ).toEqual({ dbId: "db-original", job: "ABC123", after: 3 });
  fireEvent.change(screen.getByLabelText("输出类型"), {
    target: { value: "stderr" },
  });
  expect(document.getElementById("task-log")?.dataset.filter).toBe("stderr");
  await poll();
  expect(reads).toBe(2);
});

test("file records show interrupted transfers safely without requesting other devices", async () => {
  const { page, calls } = await fixture({
    file_history: () => ({
      entries: [
        {
          time_ms: Date.now(),
          source_device_id: "remote",
          op: "push",
          path: '<img id="file-injected">',
          size: 2048,
          result: "failed_or_disconnected",
        },
      ],
      next_cursor: null,
    }),
  });
  page("任务与日志");
  fireEvent.click(screen.getByRole("button", { name: "文件与截图" }));
  await screen.findByText("失败或中断");
  expect(screen.getByText("接收文件")).toBeTruthy();
  expect(screen.getByText(/2.0 KiB/)).toBeTruthy();
  expect(document.getElementById("file-injected")).toBeNull();
  expect(screen.queryByLabelText("任务状态")).toBeNull();
  expect(calls.find((call) => call.command === "file_history")?.args).toEqual({
    before: null,
  });
});

test("late output cannot replace another task and database reset stops automatic reads", async () => {
  let completeOld!: (value: unknown) => void;
  let reads = 0;
  const pending = new Promise((resolve) => {
    completeOld = resolve;
  });
  const { page, poll } = await fixture({
    task_history: () => ({
      db_id: "db-original",
      jobs: [task("OLD"), task("NEW")],
      next_cursor: null,
    }),
    task_output: ({ job }) => {
      reads++;
      if (job === "OLD") return pending;
      throw "DB_RESET: database replaced";
    },
  });
  page("任务与日志");
  fireEvent.click(await screen.findByRole("button", { name: /OLD/ }));
  fireEvent.click(screen.getByRole("button", { name: "← 返回任务列表" }));
  fireEvent.click(await screen.findByRole("button", { name: /NEW/ }));
  await screen.findByText("DB_RESET: database replaced");
  await act(async () => {
    completeOld({
      job: task("OLD"),
      events: [event(1, "stdout", "old output")],
      has_more: false,
    });
  });
  expect(screen.getByText("任务 NEW")).toBeTruthy();
  expect(document.getElementById("task-log")?.textContent).not.toContain(
    "old output",
  );
  const previous = reads;
  await poll();
  expect(reads).toBe(previous);
});

test("remote names are text and revoked devices remain disabled during edits", async () => {
  const { page } = await fixture();
  page("设备");
  await screen.findByText('<img id="injected" src=x onerror=alert(1)>');
  expect(document.getElementById("injected")).toBeNull();
  const revoked = screen.getByLabelText(
    "允许 old-device 访问本机",
  ) as HTMLInputElement;
  expect(revoked.disabled).toBe(true);
  page("设置");
  fireEvent.change(screen.getByLabelText("默认工作目录"), {
    target: { value: "/repo" },
  });
  expect(revoked.disabled).toBe(true);
});

test("permissions require confirmation and canceled changes never reach the backend", async () => {
  const { page, calls } = await fixture();
  page("设备");
  const input = await screen.findByLabelText(
    '允许 <img id="injected" src=x onerror=alert(1)> 访问本机',
  );
  fireEvent.click(input);
  await waitFor(() =>
    expect(document.querySelector("dialog")?.open).toBe(true),
  );
  await act(async () => document.querySelector("dialog")!.close("cancel"));
  expect(calls.some((call) => call.command === "permission")).toBe(false);
  fireEvent.click(input);
  await waitFor(() =>
    expect(document.querySelector("dialog")?.open).toBe(true),
  );
  await act(async () => document.querySelector("dialog")!.close("ok"));
  expect(calls.find((call) => call.command === "permission")?.args).toEqual({
    device: "unsafe-label",
    allow: true,
  });
  expect((input as HTMLInputElement).checked).toBe(true);
});

test("joining passes the link only to join and clears it from the form", async () => {
  const { calls } = await fixture({}, false);
  const link = screen.getByLabelText("邀请链接") as HTMLInputElement;
  fireEvent.change(link, { target: { value: "xrun://test-secret" } });
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac2" },
  });
  await act(async () => {
    fireEvent.submit(document.getElementById("join-form")!);
  });
  await waitFor(() => expect(screen.queryByLabelText("邀请链接")).toBeNull());
  expect(calls.find((call) => call.command === "join")?.args).toEqual({
    link: "xrun://test-secret",
    name: "mac2",
  });
  expect(document.body.textContent).not.toContain("test-secret");
});

test("all-member access requires confirmation, respects denials and preserves individual grants", async () => {
  const { page, calls, status, poll } = await fixture();
  page("设备");
  fireEvent.click(screen.getByLabelText("允许所有设备访问本机"));
  await waitFor(() =>
    expect(document.querySelector("dialog")?.open).toBe(true),
  );
  expect(document.querySelector("dialog")?.textContent).toContain("以后加入");
  await act(async () => document.querySelector("dialog")!.close("cancel"));
  expect(calls.some((call) => call.command === "all_permissions")).toBe(false);
  fireEvent.click(screen.getByLabelText("允许所有设备访问本机"));
  await waitFor(() =>
    expect(document.querySelector("dialog")?.open).toBe(true),
  );
  await act(async () => document.querySelector("dialog")!.close("ok"));
  await waitFor(() => expect(status.local.allow_all).toBe(true));
  const individual = screen.getByLabelText(
    '允许 <img id="injected" src=x onerror=alert(1)> 访问本机',
  ) as HTMLInputElement;
  expect(individual.checked).toBe(true);
  fireEvent.click(individual);
  await waitFor(() => expect(individual.checked).toBe(false));
  status.allow_from = ["unsafe-label"];
  status.deny_from = [];
  await poll();
  fireEvent.click(screen.getByLabelText("允许所有设备访问本机"));
  await waitFor(() => expect(status.local.allow_all).toBe(false));
  expect(individual.checked).toBe(true);
});

test("pause and resume keep permissions and expose the paused state", async () => {
  const { page, calls, status } = await fixture();
  fireEvent.click(screen.getByRole("button", { name: "暂停远程访问" }));
  await screen.findByRole("button", { name: "恢复远程访问" });
  expect(status.local.remote_access_paused).toBe(true);
  page("设备");
  expect(screen.getByText(/远程访问已暂停，以下授权暂不生效/)).toBeTruthy();
  page("本机状态");
  fireEvent.click(screen.getByRole("button", { name: "恢复远程访问" }));
  await waitFor(() => expect(status.local.remote_access_paused).toBe(false));
  expect(
    calls
      .filter((call) => call.command === "pause_access")
      .map((call) => call.args),
  ).toEqual([{ paused: true }, { paused: false }]);
  expect(
    calls.some((call) =>
      ["permission", "all_permissions"].includes(call.command),
    ),
  ).toBe(false);
});

test("history pages and filters retain server cursors and stop polling while hidden", async () => {
  const { page, calls, poll } = await fixture({
    task_history: ({ before }) => ({
      db_id: "db-original",
      jobs: [task(before ? "PAGE2" : "PAGE1")],
      next_cursor: before ? null : 100,
    }),
  });
  page("任务与日志");
  await screen.findByRole("button", { name: /PAGE1/ });
  fireEvent.click(screen.getByRole("button", { name: "下一页" }));
  await screen.findByRole("button", { name: /PAGE2/ });
  expect(
    calls.filter((call) => call.command === "task_history").at(-1)?.args,
  ).toEqual({ before: 100, filter: "all" });
  fireEvent.change(screen.getByLabelText("任务状态"), {
    target: { value: "failed" },
  });
  await waitFor(() =>
    expect(
      calls.filter((call) => call.command === "task_history").at(-1)?.args,
    ).toEqual({ before: null, filter: "failed" }),
  );
  page("设置");
  const count = calls.filter((call) => call.command === "task_history").length;
  await poll();
  expect(calls.filter((call) => call.command === "task_history").length).toBe(
    count,
  );
});
