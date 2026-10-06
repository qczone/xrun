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
