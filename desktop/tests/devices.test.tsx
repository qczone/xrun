import { expect, spyOn, test } from "bun:test";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { fixture, openMemberActions } from "./fixtures";
import type { Device } from "../src/api";
import { emit } from "@tauri-apps/api/event";

test("native window hiding stops status and device polling and showing refreshes immediately", async () => {
  const { page, calls, poll } = await fixture();
  page("设备");
  await poll();
  const counts = () =>
    ["devices", "status"].map(
      (name) => calls.filter((call) => call.command === name).length,
    );
  await act(async () => {
    await emit("xrun-window-visible", false);
  });
  const hidden = counts();
  await poll();
  await poll();
  expect(counts()).toEqual(hidden);
  await act(async () => {
    await emit("xrun-window-visible", true);
  });
  expect(counts()[0]).toBeGreaterThan(hidden[0]);
  expect(counts()[1]).toBeGreaterThan(hidden[1]);
  const shown = counts();
  try {
    Object.defineProperty(document, "hidden", {
      configurable: true,
      value: true,
    });
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await poll();
    expect(counts()).toEqual(shown);
    Object.defineProperty(document, "hidden", {
      configurable: true,
      value: false,
    });
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    expect(counts()[0]).toBeGreaterThan(shown[0]);
  } finally {
    Reflect.deleteProperty(document, "hidden");
  }
});

test("menus have one owner and close on outside clicks, focus, escape and navigation", async () => {
  const { page } = await fixture();
  page("设备");
  const first = screen.getByLabelText(
    '<img id="injected" src=x onerror=alert(1)> 的更多操作',
  );
  const second = screen.getByLabelText("old-device 的更多操作");
  const isOpen = (node: HTMLElement) => node.closest("details")!.open;
  fireEvent.click(first);
  expect(isOpen(first)).toBe(true);
  fireEvent.click(second);
  expect(isOpen(first)).toBe(false);
  expect(isOpen(second)).toBe(true);
  fireEvent.pointerDown(document.body);
  expect(isOpen(second)).toBe(false);
  fireEvent.click(first);
  fireEvent.blur(first, { relatedTarget: second });
  expect(isOpen(first)).toBe(false);
  fireEvent.click(first);
  fireEvent.keyDown(first, { key: "Escape" });
  expect(isOpen(first)).toBe(false);
  expect(document.activeElement).toBe(first);
  fireEvent.click(first);
  page("本机");
  page("设备");
  expect(isOpen(first)).toBe(false);
});

test("a menu near the bottom still opens above after closing and reopening", async () => {
  const bounds = (top: number, height: number) =>
    ({
      top,
      bottom: top + height,
      height,
      left: 0,
      right: 200,
      width: 200,
    }) as DOMRect;
  spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(
    function (this: HTMLElement) {
      if (this.tagName === "MAIN") return bounds(0, 650);
      if (this.classList.contains("device-more-body")) return bounds(500, 80);
      return bounds(590, 24);
    },
  );
  const { page } = await fixture();
  page("设备");
  const summary = screen.getByLabelText("old-device 的更多操作");
  const menu = summary.closest("details")!;
  fireEvent.click(summary);
  expect(menu.dataset.placement).toBe("above");
  fireEvent.keyDown(summary, { key: "Escape" });
  fireEvent.click(summary);
  expect(menu.open).toBe(true);
  expect(menu.dataset.placement).toBe("above");
});

test("network changes clear old members and discard a previous network's pending response", async () => {
  let completeOld!: (value: unknown) => void;
  const oldRequest = new Promise((resolve) => {
    completeOld = resolve;
  });
  let delay = false;
  let network = "old";
  const member = (id: string): Device => ({
    device_id: id,
    name: id,
    online: true,
    revoked: false,
    admin: false,
    os: "linux",
  });
  const { page, status, poll, calls } = await fixture({
    devices: () =>
      delay
        ? oldRequest
        : {
            devices: [member(network === "old" ? "old-member" : "new-member")],
            server_error: null,
          },
  });
  page("设备");
  await screen.findByLabelText("允许 old-member 访问本机");
  delay = true;
  await poll();
  const count = calls.filter((call) => call.command === "devices").length;
  network = "new";
  delay = false;
  status.network!.network_id = "net-new";
  await poll();
  await screen.findByLabelText("允许 new-member 访问本机");
  expect(
    calls.filter((call) => call.command === "devices").length,
  ).toBeGreaterThan(count);
  expect(screen.queryByLabelText("允许 old-member 访问本机")).toBeNull();
  await act(async () =>
    completeOld({ devices: [member("late-old-member")], server_error: null }),
  );
  expect(screen.queryByLabelText("允许 late-old-member 访问本机")).toBeNull();
  expect(screen.getByLabelText("允许 new-member 访问本机")).toBeTruthy();
});

test("device polling stops while hidden or confirming and resumes when visible", async () => {
  const { page, calls, poll } = await fixture();
  const count = () => calls.filter((call) => call.command === "devices").length;
  const hiddenCount = count();
  await poll();
  expect(count()).toBe(hiddenCount);
  page("设备");
  await poll();
  expect(count()).toBeGreaterThan(hiddenCount);
  fireEvent.click(
    screen.getByLabelText(
      '允许 <img id="injected" src=x onerror=alert(1)> 访问本机',
    ),
  );
  const confirmingCount = count();
  await poll();
  expect(count()).toBe(confirmingCount);
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close(),
  );
  await poll();
  expect(count()).toBeGreaterThan(confirmingCount);
  page("设置");
  const finalCount = count();
  await poll();
  expect(count()).toBe(finalCount);
});

test("escape after an accepted confirmation cannot accept the next request", async () => {
  const { page, calls } = await fixture();
  page("设备");
  fireEvent.click(
    screen.getByLabelText(
      '允许 <img id="injected" src=x onerror=alert(1)> 访问本机',
    ),
  );
  const dialog = document.getElementById("confirm") as HTMLDialogElement;
  await act(async () => dialog.close("ok"));
  expect(calls.filter((call) => call.command === "permission")).toHaveLength(1);
  fireEvent.click(screen.getByText("高级访问策略"));
  fireEvent.click(screen.getByLabelText("允许全体成员访问本机"));
  expect(dialog.open).toBe(true);
  expect(dialog.returnValue).toBe("");
  await act(async () => dialog.close());
  expect(
    calls.filter((call) => call.command === "all_permissions"),
  ).toHaveLength(0);
});

test("unmounting a pending confirmation cancels the operation", async () => {
  const { page, calls, unmount } = await fixture();
  page("设备");
  fireEvent.click(
    screen.getByLabelText(
      '允许 <img id="injected" src=x onerror=alert(1)> 访问本机',
    ),
  );
  expect((document.getElementById("confirm") as HTMLDialogElement).open).toBe(
    true,
  );
  unmount();
  await act(async () => {});
  expect(calls.filter((call) => call.command === "permission")).toHaveLength(0);
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
