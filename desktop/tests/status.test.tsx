import { expect, test } from "bun:test";
import {
  act,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { fixture } from "./fixtures";
import type { Status } from "../src/api";
test("network role and relay addresses remain visible when the relay is offline", async () => {
  const { status, poll, page } = await fixture({
    devices: () => ({
      devices: null,
      server_error: { code: "CONNECT_FAILED", message: "relay offline" },
    }),
  });
  status.local.daemon_connected = false;
  await poll();
  expect(screen.getByText(/本机 · 普通设备/)).toBeTruthy();
  await page("设置");
  fireEvent.click(screen.getByText("网络与服务", { selector: "summary span" }));
  expect(screen.getByText("manager1")).toBeTruthy();
  expect(screen.getByText("https://relay.example:8080")).toBeTruthy();
  status.network!.is_manager = true;
  status.network!.manager_name = "mac1";
  await poll();
  expect(
    screen.getByText(/本机 · 管理设备/, {
      selector: ".local-device-name > span",
    }),
  ).toBeTruthy();
  await page("设备");
  expect(screen.getByRole("button", { name: "邀请设备" })).toBeTruthy();
});

test("macOS dev mode allows daemon start but disables App login startup", async () => {
  const { calls, status, poll, page } = await fixture();
  status.service.development = true;
  status.service.installed = false;
  status.local.daemon_running = false;
  status.local.daemon_connected = false;
  await poll();
  await page("设置");
  expect(
    (
      screen.getByRole("checkbox", {
        name: /^登录时显示 xrun 图标/,
      }) as HTMLInputElement
    ).disabled,
  ).toBe(true);
  fireEvent.click(screen.getByText("网络与服务", { selector: "summary span" }));
  expect(
    screen.getByText("开发模式手动启动，退出 App 后仍继续运行"),
  ).toBeTruthy();
  fireEvent.click(
    within(document.getElementById("network-settings")!).getByRole("button", {
      name: "启动服务",
    }),
  );
  await waitFor(() =>
    expect(calls.some((call) => call.command === "start")).toBe(true),
  );
  expect(calls.some((call) => call.command === "autostart")).toBe(false);
});

test("pause and resume keep permissions and expose the paused state", async () => {
  const { page, calls, status } = await fixture();
  fireEvent.click(screen.getByRole("button", { name: "暂停访问" }));
  await screen.findByRole("button", { name: "恢复访问" });
  expect(status.local.remote_access_paused).toBe(true);
  await page("设备");
  expect(
    screen.getByText(/已暂停/, { selector: ".access-paused" }),
  ).toBeTruthy();
  await page("设备");
  fireEvent.click(screen.getByRole("button", { name: "恢复访问" }));
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

test("recovered status errors disappear and dismissed errors do not reappear on every poll", async () => {
  const { status, poll } = await fixture();
  status.error = {
    code: "CONNECT_FAILED",
    message: "temporary network failure",
  };
  await poll();
  expect(screen.getByRole("alert").textContent).toContain("本机检查遇到问题");
  status.error = null;
  await poll();
  expect(screen.queryByRole("alert")).toBeNull();
  status.error = {
    code: "CONNECT_FAILED",
    message: "temporary network failure",
  };
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
  status.error = {
    code: "CONNECT_FAILED",
    message: "temporary network failure",
  };
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
  fireEvent.click(screen.getByRole("button", { name: "启动服务" }));
  await screen.findByText("后台服务未能启动，请查看详情后重试。");
  await poll();
  expect(screen.getByRole("alert").textContent).toContain("HELPER_NOT_FOUND");
  await page("设置");
  expect(screen.queryByRole("alert")).toBeNull();
  await page("设备");
  fireEvent.click(
    within(screen.getByRole("alert")).getByRole("button", {
      name: "关闭错误提示",
    }),
  );
  await poll();
  expect(screen.queryByRole("alert")).toBeNull();
});

test("global service controls surface errors on the activity page", async () => {
  const { page } = await fixture({
    pause_access: () => {
      throw "STORAGE_ERROR: permission state unavailable";
    },
  });
  await page("活动");
  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "暂停访问" }));
  });
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toContain("远程访问设置未能更新");
  expect(alert.textContent).toContain("STORAGE_ERROR");
  await page("设备");
  expect(screen.queryByRole("alert")).toBeNull();
  await page("活动");
  expect(screen.getByRole("alert").textContent).toContain("STORAGE_ERROR");
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
  const start = screen.getByRole("button", { name: "启动服务" });
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
