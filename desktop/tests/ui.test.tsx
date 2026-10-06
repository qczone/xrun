import { expect, spyOn, test } from "bun:test";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
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
      version: "0.0.1-beta.2",
      daemon_running: true,
      daemon_connected: true,
      remote_access_paused: false,
      allow_all: false,
      daemon_installed: true,
    },
    network: joined
      ? {
          network_id: "net-test",
          manager_id: "manager",
          manager_name: "manager1",
          is_manager: false,
          relay_addresses: ["https://relay.example:8080"],
        }
      : null,
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
      admin: false,
      os: "windows",
    },
    {
      device_id: "revoked",
      name: "old-device",
      online: false,
      revoked: true,
      admin: false,
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
  return { ...view, calls, status, settings, devices, poll, page };
}

function openMemberActions(name: string) {
  const summary = screen.getByLabelText(`${name} 的更多操作`);
  if (!summary.closest("details")!.open) fireEvent.click(summary);
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

test("network role and relay addresses remain visible when the relay is offline", async () => {
  const { status, poll, page } = await fixture({
    devices: () => ({
      devices: null,
      server_error: { code: "CONNECT_FAILED", message: "relay offline" },
    }),
  });
  status.local.daemon_connected = false;
  await poll();
  expect(screen.getByText("普通设备")).toBeTruthy();
  expect(screen.getByText("manager1")).toBeTruthy();
  expect(screen.getByText("https://relay.example:8080")).toBeTruthy();
  status.network!.is_manager = true;
  status.network!.manager_name = "mac1";
  await poll();
  expect(
    screen.getByText("管理设备", { selector: ".overview-identity span" }),
  ).toBeTruthy();
  page("设备");
  expect(screen.getByText(/管理网络成员/)).toBeTruthy();
});
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
        name: /^登录时显示 xrun 图标/,
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
  const input = screen.getByLabelText("同时运行的任务数") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "8" } });
  await poll();
  page("本机");
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
  page("活动记录");
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
  page("活动记录");
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
  page("活动记录");
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
  expect(screen.getByText("任务 NEW · 查看详情")).toBeTruthy();
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
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("cancel"),
  );
  expect(calls.some((call) => call.command === "permission")).toBe(false);
  fireEvent.click(input);
  await waitFor(() =>
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("ok"),
  );
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

test("network creation clears secrets, prevents duplicate submissions and opens member management", async () => {
  let complete!: () => void;
  const pending = new Promise<void>((resolve) => {
    complete = resolve;
  });
  let current!: Status;
  const { calls, status } = await fixture(
    {
      create_network: async () => {
        await pending;
        current.local.joined = true;
        current.network = {
          network_id: "new-network",
          manager_id: "self",
          manager_name: "mac2",
          is_manager: true,
          relay_addresses: ["https://relay.example:8080"],
        };
      },
    },
    false,
  );
  current = status;
  fireEvent.change(screen.getByLabelText("邀请链接"), {
    target: { value: "xrun://old-secret" },
  });
  fireEvent.click(screen.getByRole("button", { name: "创建新网络" }));
  const link = screen.getByLabelText("中转部署链接") as HTMLInputElement;
  expect(link.value).toBe("");
  fireEvent.change(link, { target: { value: "xrun-relay://creation-secret" } });
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac2" },
  });
  fireEvent.submit(document.getElementById("create-network-form")!);
  fireEvent.submit(document.getElementById("create-network-form")!);
  expect(
    calls.filter((call) => call.command === "create_network"),
  ).toHaveLength(1);
  await act(async () => complete());
  await waitFor(() =>
    expect(screen.queryByLabelText("中转部署链接")).toBeNull(),
  );
  expect(
    document
      .querySelector('[data-page="devices"]')
      ?.getAttribute("aria-current"),
  ).toBe("page");
  expect(calls.find((call) => call.command === "create_network")?.args).toEqual(
    {
      link: "xrun-relay://creation-secret",
      name: "mac2",
    },
  );
  expect(document.body.textContent).not.toContain("creation-secret");
  expect(calls.some((call) => call.command === "join")).toBe(false);
});

test("creation can retry after an identity was saved but publication failed", async () => {
  let current!: Status;
  let attempts = 0;
  const { calls, status } = await fixture(
    {
      create_network: () => {
        current.local.joined = true;
        if (++attempts === 1) throw "CONNECT_FAILED: relay unavailable";
      },
    },
    false,
  );
  current = status;
  fireEvent.click(screen.getByRole("button", { name: "创建新网络" }));
  fireEvent.change(screen.getByLabelText("中转部署链接"), {
    target: { value: "xrun-relay://retry-secret" },
  });
  fireEvent.change(screen.getByLabelText("本机名称"), {
    target: { value: "mac2" },
  });
  fireEvent.submit(document.getElementById("create-network-form")!);
  await screen.findByRole("button", { name: "重试创建网络" });
  expect(
    (screen.getByLabelText("中转部署链接") as HTMLInputElement).value,
  ).toBe("xrun-relay://retry-secret");
  fireEvent.submit(document.getElementById("create-network-form")!);
  await waitFor(() =>
    expect(screen.queryByLabelText("中转部署链接")).toBeNull(),
  );
  expect(
    calls.filter((call) => call.command === "create_network"),
  ).toHaveLength(2);
});

test("only managers generate invitations, default registration has no grant and links copy only on request", async () => {
  const { calls, status, poll, page } = await fixture({
    invite: ({ allow }) => ({
      link: "xrun://invite-secret",
      allow,
      expires_in: 600,
    }),
  });
  page("设备");
  expect(screen.queryByRole("button", { name: "生成邀请链接" })).toBeNull();
  status.network!.is_manager = true;
  await poll();
  fireEvent.click(screen.getByRole("button", { name: "邀请新设备" }));
  const option = screen.getByLabelText(
    "允许新设备与本机互相访问",
  ) as HTMLInputElement;
  expect(option.checked).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "生成邀请链接" }));
  await screen.findByLabelText("生成的邀请链接");
  expect(calls.find((call) => call.command === "invite")?.args).toEqual({
    allow: false,
  });
  expect(calls.some((call) => call.command === "copy_invitation")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "复制链接" }));
  await screen.findByText("邀请链接已复制");
  expect(
    calls.find((call) => call.command === "copy_invitation")?.args,
  ).toEqual({ link: "xrun://invite-secret" });
  page("本机");
  expect(screen.queryByLabelText("生成的邀请链接")).toBeNull();
  page("设备");
  expect(screen.queryByLabelText("生成的邀请链接")).toBeNull();
});

test("mutual invitation grants require confirmation and expired links are cleared", async () => {
  const { calls, status, poll, page } = await fixture({
    invite: ({ allow }) => ({
      link: "xrun://short-lived",
      allow,
      expires_in: 0,
    }),
  });
  status.network!.is_manager = true;
  await poll();
  page("设备");
  fireEvent.click(screen.getByRole("button", { name: "邀请新设备" }));
  fireEvent.click(screen.getByLabelText("允许新设备与本机互相访问"));
  fireEvent.click(screen.getByRole("button", { name: "生成邀请链接" }));
  await waitFor(() =>
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("cancel"),
  );
  expect(calls.some((call) => call.command === "invite")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "生成邀请链接" }));
  await waitFor(() =>
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("ok"),
  );
  await screen.findByText("链接已过期，请重新生成。");
  expect(screen.queryByLabelText("生成的邀请链接")).toBeNull();
  expect(calls.find((call) => call.command === "invite")?.args).toEqual({
    allow: true,
  });
});

test("an invitation completed after leaving its page cannot expose a secret", async () => {
  let complete!: (value: unknown) => void;
  const pending = new Promise((resolve) => {
    complete = resolve;
  });
  const { status, poll, page } = await fixture({ invite: () => pending });
  status.network!.is_manager = true;
  await poll();
  page("设备");
  fireEvent.click(screen.getByRole("button", { name: "邀请新设备" }));
  fireEvent.click(screen.getByRole("button", { name: "生成邀请链接" }));
  page("本机");
  await act(async () =>
    complete({ link: "xrun://late-secret", allow: false, expires_in: 600 }),
  );
  page("设备");
  expect(screen.queryByLabelText("生成的邀请链接")).toBeNull();
  expect(document.body.textContent).not.toContain("late-secret");
});

test("membership revocation is manager-only, requires confirmation and sends the immutable ID", async () => {
  let members!: Device[];
  const { calls, status, devices, poll, page } = await fixture({
    revoke: ({ device }) => {
      members.find((entry) => entry.device_id === device)!.revoked = true;
      return {
        device_id: device,
        revoked: true,
        roster_version: 2,
        undelivered: [],
        sync_error: null,
      };
    },
  });
  members = devices;
  page("设备");
  expect(screen.queryByRole("button", { name: /^撤销 / })).toBeNull();
  status.network!.is_manager = true;
  await poll();
  const name = '撤销 <img id="injected" src=x onerror=alert(1)> 的成员身份';
  openMemberActions('<img id="injected" src=x onerror=alert(1)>');
  fireEvent.click(screen.getByRole("button", { name }));
  await waitFor(() =>
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("cancel"),
  );
  expect(calls.some((call) => call.command === "revoke")).toBe(false);
  openMemberActions('<img id="injected" src=x onerror=alert(1)>');
  fireEvent.click(screen.getByRole("button", { name }));
  await waitFor(() =>
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("ok"),
  );
  await screen.findByText("当前其他成员均已确认收到更新。");
  expect(calls.find((call) => call.command === "revoke")?.args).toEqual({
    device: "unsafe-label",
  });
  expect(screen.queryByRole("button", { name })).toBeNull();
  expect(
    (
      screen.getByLabelText(
        '允许 <img id="injected" src=x onerror=alert(1)> 访问本机',
      ) as HTMLInputElement
    ).disabled,
  ).toBe(true);
  expect(document.getElementById("injected")).toBeNull();
});

test("revocation keeps its local result during relay failure and can retry delivery", async () => {
  let offline = false;
  let attempts = 0;
  const members: Device[] = [
    {
      device_id: "win-id",
      name: "win1",
      online: true,
      revoked: false,
      admin: false,
      os: "windows",
    },
    {
      device_id: "cloud-id",
      name: "cloud1",
      online: false,
      revoked: false,
      admin: false,
      os: "linux",
    },
  ];
  const { calls, status, poll, page } = await fixture({
    devices: () =>
      offline
        ? {
            devices: null,
            server_error: { code: "CONNECT_FAILED", message: "offline" },
          }
        : { devices: members, server_error: null },
    revoke: ({ device }) => {
      members[0].revoked = true;
      const first = ++attempts === 1;
      offline = first;
      return {
        device_id: device,
        revoked: true,
        roster_version: 2,
        undelivered: first ? ["cloud-id", "unknown-id"] : [],
        sync_error: first ? "CONNECT_FAILED: relay unavailable" : null,
      };
    },
  });
  status.network!.is_manager = true;
  await poll();
  page("设备");
  openMemberActions("win1");
  fireEvent.click(screen.getByRole("button", { name: "撤销 win1 的成员身份" }));
  await waitFor(() =>
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("ok"),
  );
  await screen.findByText("已撤销 win1");
  const result = document.querySelector(".revocation-result")!;
  expect(result.textContent).toContain("成员名单同步失败");
  expect(result.textContent).toContain("cloud1");
  expect(result.textContent).toContain("unknown-id");
  expect(screen.queryByText("当前其他成员均已确认收到更新。")).toBeNull();
  expect(
    (screen.getByLabelText("允许 win1 访问本机") as HTMLInputElement).disabled,
  ).toBe(true);
  fireEvent.click(screen.getByRole("button", { name: "重新同步撤销记录" }));
  await screen.findByText("当前其他成员均已确认收到更新。");
  expect(
    calls.filter((call) => call.command === "revoke").map((call) => call.args),
  ).toEqual([{ device: "win-id" }, { device: "win-id" }]);
});

test("backend refusal does not falsely mark a member revoked", async () => {
  const { status, poll, page } = await fixture({
    revoke: () => {
      throw "NOT_MANAGER: only the manager may revoke";
    },
  });
  status.network!.is_manager = true;
  await poll();
  page("设备");
  const name = '撤销 <img id="injected" src=x onerror=alert(1)> 的成员身份';
  openMemberActions('<img id="injected" src=x onerror=alert(1)>');
  fireEvent.click(screen.getByRole("button", { name }));
  await waitFor(() =>
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("ok"),
  );
  await screen.findByText("NOT_MANAGER: only the manager may revoke");
  expect(document.querySelector(".revocation-result")).toBeNull();
  openMemberActions('<img id="injected" src=x onerror=alert(1)>');
  expect(screen.getByRole("button", { name })).toBeTruthy();
});

test("all-member access requires confirmation, respects denials and preserves individual grants", async () => {
  const { page, calls, status, poll } = await fixture();
  page("设备");
  fireEvent.click(screen.getByText("高级访问策略"));
  fireEvent.click(screen.getByLabelText("允许全体成员访问本机"));
  await waitFor(() =>
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  expect(
    (document.getElementById("confirm") as HTMLDialogElement)?.textContent,
  ).toContain("以后加入");
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("cancel"),
  );
  expect(calls.some((call) => call.command === "all_permissions")).toBe(false);
  fireEvent.click(screen.getByLabelText("允许全体成员访问本机"));
  await waitFor(() =>
    expect(
      (document.getElementById("confirm") as HTMLDialogElement)?.open,
    ).toBe(true),
  );
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("ok"),
  );
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
  fireEvent.click(screen.getByLabelText("允许全体成员访问本机"));
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
  page("本机");
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
  page("活动记录");
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

test("recovered status errors disappear and dismissed errors do not reappear on every poll", async () => {
  const { status, poll } = await fixture();
  status.error = "CONNECT_FAILED: temporary network failure";
  await poll();
  expect(screen.getByRole("alert").textContent).toContain("本机检查遇到问题");
  status.error = null;
  await poll();
  expect(screen.queryByRole("alert")).toBeNull();
  status.error = "CONNECT_FAILED: temporary network failure";
  await poll();
  fireEvent.click(
    within(screen.getByRole("alert")).getByRole("button", {
      name: "关闭错误提示",
    }),
  );
  await poll();
  expect(screen.queryByRole("alert")).toBeNull();
  status.error = null;
  await poll();
  status.error = "CONNECT_FAILED: temporary network failure";
  await poll();
  expect(screen.getByRole("alert")).toBeTruthy();
});

test("operation errors stay on the originating page until dismissed or retried", async () => {
  const { status, poll, page } = await fixture({
    start: () => {
      throw "HELPER_NOT_FOUND: missing helper";
    },
  });
  status.local.daemon_running = false;
  status.local.daemon_connected = false;
  await poll();
  fireEvent.click(screen.getByRole("button", { name: "启动后台服务" }));
  await screen.findByText("后台服务未能启动，请查看详情后重试。");
  await poll();
  expect(screen.getByRole("alert").textContent).toContain("HELPER_NOT_FOUND");
  page("设置");
  expect(screen.queryByRole("alert")).toBeNull();
  page("本机");
  fireEvent.click(
    within(screen.getByRole("alert")).getByRole("button", {
      name: "关闭错误提示",
    }),
  );
  await poll();
  expect(screen.queryByRole("alert")).toBeNull();
});

test("service startup displays progress and prevents duplicate requests", async () => {
  let complete!: () => void;
  let current!: Status;
  const pending = new Promise<void>((resolve) => {
    complete = resolve;
  });
  const { status, calls, poll } = await fixture({
    start: async () => {
      await pending;
      current.local.daemon_running = true;
    },
  });
  current = status;
  status.local.daemon_running = false;
  status.local.daemon_connected = false;
  await poll();
  const start = screen.getByRole("button", { name: "启动后台服务" });
  fireEvent.click(start);
  fireEvent.click(start);
  expect(
    (
      (await screen.findByRole("button", {
        name: "正在启动…",
      })) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  expect(calls.filter((call) => call.command === "start")).toHaveLength(1);
  await act(async () => complete());
  await screen.findByText("后台服务已启动");
});

for (const command of ["join", "create_network"] as const) {
  test(`${command} clears the link after registration and retries only service startup`, async () => {
    let current!: Status;
    const { calls, status } = await fixture(
      {
        [command]: () => {
          current.local.joined = true;
          current.local.daemon_running = false;
          current.local.daemon_connected = false;
          current.error = "SERVICE_START_FAILED: system approval required";
          throw current.error;
        },
        start: () => {
          current.local.daemon_running = true;
          current.error = null;
        },
      },
      false,
    );
    current = status;
    if (command === "create_network")
      fireEvent.click(screen.getByRole("button", { name: "创建新网络" }));
    fireEvent.change(
      screen.getByLabelText(command === "join" ? "邀请链接" : "中转部署链接"),
      { target: { value: "xrun://registered-secret" } },
    );
    fireEvent.change(screen.getByLabelText("本机名称"), {
      target: { value: "mac2" },
    });
    fireEvent.submit(
      document.getElementById(
        command === "join" ? "join-form" : "create-network-form",
      )!,
    );
    await screen.findByText(
      command === "join"
        ? "已加入网络，后台服务未启动。请启动后台服务。"
        : "网络已创建，后台服务未启动。请启动后台服务。",
    );
    await waitFor(() =>
      expect(
        screen.queryByLabelText(
          command === "join" ? "邀请链接" : "中转部署链接",
        ),
      ).toBeNull(),
    );
    expect(document.body.textContent).not.toContain("registered-secret");
    fireEvent.click(screen.getByRole("button", { name: "启动后台服务" }));
    await screen.findByText("后台服务已启动");
    expect(calls.filter((call) => call.command === command)).toHaveLength(1);
    expect(calls.filter((call) => call.command === "start")).toHaveLength(1);
    expect(screen.queryByRole("alert")).toBeNull();
  });
}

test("closing the invitation dialog discards a result that arrives later", async () => {
  let complete!: (value: unknown) => void;
  const pending = new Promise((resolve) => {
    complete = resolve;
  });
  const { status, poll, page } = await fixture({ invite: () => pending });
  status.network!.is_manager = true;
  await poll();
  page("设备");
  fireEvent.click(screen.getByRole("button", { name: "邀请新设备" }));
  fireEvent.click(await screen.findByRole("button", { name: "生成邀请链接" }));
  await screen.findByRole("button", { name: "正在生成…" });
  fireEvent.click(screen.getByRole("button", { name: "关闭邀请" }));
  await waitFor(() =>
    expect(
      (document.getElementById("invite-dialog") as HTMLDialogElement).open,
    ).toBe(false),
  );
  await act(async () =>
    complete({
      link: "xrun://closed-dialog-secret",
      allow: false,
      expires_in: 600,
    }),
  );
  fireEvent.click(screen.getByRole("button", { name: "邀请新设备" }));
  await screen.findByRole("button", { name: "生成邀请链接" });
  expect(screen.queryByLabelText("生成的邀请链接")).toBeNull();
  expect(document.body.textContent).not.toContain("closed-dialog-secret");
});

test("scrolling up pauses automatic scrolling while new output continues to arrive", async () => {
  let reads = 0;
  const { page, poll } = await fixture({
    task_history: () => ({
      db_id: "db-original",
      jobs: [task()],
      next_cursor: null,
    }),
    task_output: () => ({
      job: task(),
      has_more: false,
      events: [event(++reads, "stdout", `output ${reads}\n`)],
    }),
  });
  page("活动记录");
  fireEvent.click(await screen.findByRole("button", { name: /ABC123/ }));
  await waitFor(() =>
    expect(document.getElementById("task-log")?.textContent).toContain(
      "output 1",
    ),
  );
  const log = document.getElementById("task-log")!;
  Object.defineProperty(log, "scrollHeight", {
    configurable: true,
    value: 1000,
  });
  Object.defineProperty(log, "clientHeight", {
    configurable: true,
    value: 200,
  });
  log.scrollTop = 800;
  fireEvent.scroll(log);
  log.scrollTop = 500;
  fireEvent.scroll(log);
  const follow = screen.getByLabelText("自动滚动到底部") as HTMLInputElement;
  expect(follow.checked).toBe(false);
  await poll();
  expect(log.textContent).toContain("output 2");
  expect(log.scrollTop).toBe(500);
  fireEvent.click(screen.getByRole("button", { name: "回到最新输出" }));
  expect(follow.checked).toBe(true);
  expect(log.scrollTop).toBeGreaterThanOrEqual(800);
});

test("returning from task details retains the current page and list scroll position", async () => {
  const { calls, page } = await fixture({
    task_history: ({ before }) => ({
      db_id: "db-original",
      jobs: [task(before ? "PAGE2" : "PAGE1")],
      next_cursor: before ? null : 100,
    }),
    task_output: () => ({ job: task("PAGE2"), has_more: false, events: [] }),
  });
  page("活动记录");
  await screen.findByRole("button", { name: /PAGE1/ });
  fireEvent.click(screen.getByRole("button", { name: "下一页" }));
  const row = await screen.findByRole("button", { name: /PAGE2/ });
  const list = document.querySelector(".history-record-panel")!;
  list.scrollTop = 120;
  fireEvent.click(row);
  fireEvent.click(
    await screen.findByRole("button", { name: "← 返回任务列表" }),
  );
  await screen.findByRole("button", { name: /PAGE2/ });
  expect(list.scrollTop).toBe(120);
  expect(screen.getByText("第 2 页")).toBeTruthy();
  expect(
    calls.filter((call) => call.command === "task_history").at(-1)?.args,
  ).toEqual({ before: 100, filter: "all" });
});
