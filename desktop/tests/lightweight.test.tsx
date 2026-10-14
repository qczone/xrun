import { expect, test } from "bun:test";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import { fixture } from "./fixtures";

const requestRelease = async (generation = 1) => {
  await act(async () => {
    await emit("xrun-lightweight-request", generation);
  });
};

test("lightweight settings default to 30 seconds, persist without joining, and allow manual entry", async () => {
  const { calls, page } = await fixture({}, false);
  await page("设置");
  const automatic = screen.getByLabelText("自动轻量模式") as HTMLInputElement;
  expect(automatic.checked).toBe(true);
  const delay = screen.getByRole("combobox", { name: "进入延迟" });
  expect(delay.textContent).toContain("30 秒");
  fireEvent.keyDown(delay, { key: "ArrowDown" });
  fireEvent.click(await screen.findByRole("option", { name: "1 分钟" }));
  await waitFor(() => expect(delay.textContent).toContain("1 分钟"));
  expect(
    calls.find((call) => call.command === "save_lightweight_settings")?.args,
  ).toEqual({
    preferences: { enabled: true, delay_seconds: 60 },
  });
  await waitFor(() => expect(automatic.disabled).toBe(false));
  fireEvent.click(automatic);
  await waitFor(() => expect(automatic.checked).toBe(false));
  expect(delay.getAttribute("disabled")).not.toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "立即进入" }));
  await waitFor(() =>
    expect(calls.some((call) => call.command === "enter_lightweight")).toBe(
      true,
    ),
  );
  expect(
    calls.some((call) => ["stop", "remove_service"].includes(call.command)),
  ).toBe(false);
});

test("release acknowledges the native generation with the current page exactly once", async () => {
  const { calls, page } = await fixture();
  await page("活动");
  await requestRelease(41);
  expect(calls.filter((call) => call.command === "release_window")).toEqual([
    { command: "release_window", args: { generation: 41, page: "activity" } },
  ]);
});

test("an open confirmation dialog defers release without confirming its action", async () => {
  const { calls, page } = await fixture();
  await page("设置");
  fireEvent.click(screen.getByText("网络与服务", { selector: "summary span" }));
  fireEvent.click(screen.getByRole("button", { name: "停止服务" }));
  await screen.findByRole("heading", { name: "停止后台服务？" });
  await requestRelease();
  expect(calls.some((call) => call.command === "release_window")).toBe(false);
  expect(calls.some((call) => call.command === "stop")).toBe(false);
  await act(async () =>
    (screen.getByRole("dialog") as HTMLDialogElement).close("cancel"),
  );
  await requestRelease(2);
  expect(calls.some((call) => call.command === "release_window")).toBe(true);
});

test("a native directory chooser protects the window until its result is delivered", async () => {
  let complete!: (path: string | null) => void;
  const directory = new Promise<string | null>((resolve) => {
    complete = resolve;
  });
  const { calls, page } = await fixture({ choose_directory: () => directory });
  await page("设置");
  fireEvent.click(screen.getByText("执行环境", { selector: "summary span" }));
  fireEvent.click(screen.getByRole("button", { name: "选择" }));
  await waitFor(() =>
    expect(calls.some((call) => call.command === "choose_directory")).toBe(
      true,
    ),
  );
  await requestRelease();
  expect(calls.some((call) => call.command === "release_window")).toBe(false);
  await act(async () => {
    complete(null);
  });
  await requestRelease(2);
  expect(calls.some((call) => call.command === "release_window")).toBe(true);
});

test("unsaved execution and retention settings defer release until both are saved", async () => {
  const { calls, page } = await fixture();
  await page("设置");
  fireEvent.click(screen.getByText("执行环境", { selector: "summary span" }));
  fireEvent.change(screen.getByLabelText("默认工作目录"), {
    target: { value: "/unsaved" },
  });
  fireEvent.change(screen.getByLabelText("附件保留时间（天）"), {
    target: { value: "7" },
  });
  await requestRelease();
  expect(calls.some((call) => call.command === "release_window")).toBe(false);
  fireEvent.submit(document.getElementById("execution-form")!);
  await screen.findByText("执行环境已保存");
  await requestRelease();
  expect(calls.some((call) => call.command === "release_window")).toBe(false);
  fireEvent.submit(document.getElementById("retention-form")!);
  await screen.findByText("附件保留时间已保存");
  await requestRelease(2);
  expect(calls.filter((call) => call.command === "release_window")).toEqual([
    { command: "release_window", args: { generation: 2, page: "settings" } },
  ]);
});

test("authorization and account selection protect the webview across separate IPC calls", async () => {
  let complete!: (accounts: { id: string; name: string }[]) => void;
  const accounts = new Promise<{ id: string; name: string }[]>((resolve) => {
    complete = resolve;
  });
  const { calls } = await fixture(
    { cloudflare_accounts: () => accounts },
    false,
  );
  fireEvent.click(screen.getByRole("button", { name: "新建网络" }));
  fireEvent.submit(document.getElementById("cloudflare-create-form")!);
  await waitFor(() =>
    expect(calls.some((call) => call.command === "cloudflare_accounts")).toBe(
      true,
    ),
  );
  await requestRelease();
  expect(calls.some((call) => call.command === "release_window")).toBe(false);
  await act(async () => {
    complete([
      { id: "a".repeat(32), name: "Personal" },
      { id: "b".repeat(32), name: "Team" },
    ]);
  });
  await screen.findByLabelText("Cloudflare 账户");
  await requestRelease();
  expect(calls.some((call) => call.command === "release_window")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "接入码" }));
  await requestRelease(2);
  expect(calls.filter((call) => call.command === "release_window")).toEqual([
    { command: "release_window", args: { generation: 2, page: "devices" } },
  ]);
});

test("failed preference writes preserve the previous controls and show an actionable error", async () => {
  const { page } = await fixture({
    save_lightweight_settings: () =>
      Promise.reject({ code: "STORAGE_ERROR", message: "read-only settings" }),
  });
  await page("设置");
  const automatic = screen.getByLabelText("自动轻量模式") as HTMLInputElement;
  fireEvent.click(automatic);
  await screen.findByText("轻量模式设置未能保存");
  expect(automatic.checked).toBe(true);
});
