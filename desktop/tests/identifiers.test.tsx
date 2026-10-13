import { expect, test } from "bun:test";
import { act, fireEvent, screen, within } from "@testing-library/react";
import {
  fixture,
  commandJob,
  findActivity,
  openMemberActions,
} from "./fixtures";

const firstId = "9db79d9bfad74bfe9c2fc01bc6fc79e1";
const secondId = "9db79d9bfad74bfe9c2fc01bc6fc79e2";
const networkId = "net_mdqe34dt4yiq7gwnwxv3hyclztl2g6v5yf4bvktmplnh2jmquxza";

test("compact device IDs distinguish matching prefixes while copying and granting use the full ID", async () => {
  const members = [firstId, secondId].map((device_id, index) => ({
    device_id,
    name: `build-${index + 1}`,
    online: true,
    revoked: false,
    admin: false,
    os: "windows",
    arch: "aarch64",
    daemon_version: "0.1.0-rc.8",
  }));
  const { calls } = await fixture({
    devices: () => ({ devices: members, server_error: null }),
  });
  const list = document.getElementById("devices")!;
  expect(list.querySelectorAll(".device-info code")[0]?.textContent).toBe(
    "9db79d9b…79e1",
  );
  expect(list.querySelectorAll(".device-info code")[1]?.textContent).toBe(
    "9db79d9b…79e2",
  );
  expect(list.textContent).not.toContain("dev_");
  expect(list.textContent).not.toContain("系统未知");
  expect(list.querySelector(".device-info")?.textContent).toContain(
    "Windows · ARM64",
  );
  const search = screen.getByRole("textbox", { name: "搜索设备" });
  fireEvent.change(search, { target: { value: secondId } });
  expect(within(list).queryByText("build-1")).toBeNull();
  expect(within(list).getByText("build-2")).toBeTruthy();
  fireEvent.change(search, { target: { value: "9db79d9b…79e2" } });
  expect(within(list).getByText("build-2")).toBeTruthy();
  openMemberActions("build-2");
  expect(
    screen.getByText("0.1.0-rc.8", { selector: ".device-facts dd" }),
  ).toBeTruthy();
  expect(calls.some((call) => call.command === "copy_identifier")).toBe(false);
  fireEvent.click(screen.getByRole("button", { name: "复制设备 ID" }));
  await screen.findByText("完整 ID 已复制");
  expect(
    calls.find((call) => call.command === "copy_identifier")?.args,
  ).toEqual({
    value: secondId,
  });
  fireEvent.click(screen.getByLabelText("允许 build-2 访问本机"));
  await act(async () =>
    (document.getElementById("confirm") as HTMLDialogElement).close("ok"),
  );
  expect(calls.find((call) => call.command === "permission")?.args).toEqual({
    device: secondId,
    allow: true,
  });
});

test("cached device information is shown when the relay query fails after reopening", async () => {
  const { calls } = await fixture({
    devices: () => ({
      devices: [
        {
          device_id: firstId,
          name: "windows-pc",
          online: false,
          revoked: false,
          admin: false,
          os: "windows",
          arch: "x86_64",
          daemon_version: "0.1.0-rc.8",
          last_seen: 1000,
        },
      ],
      server_error: { code: "CONNECTION_CLOSED", message: "relay unavailable" },
    }),
  });
  const name = await screen.findByText("windows-pc", {
    selector: ".device-name",
  });
  const row = name.closest(".device-row")!;
  expect(row.querySelector(".device-info")?.textContent).toContain(
    "Windows · x64",
  );
  expect(within(row as HTMLElement).getByText("离线")).toBeTruthy();
  await screen.findByText("CONNECTION_CLOSED: relay unavailable");
  openMemberActions("windows-pc");
  expect(screen.getByText("最近在线")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "复制设备 ID" }));
  await screen.findByText("完整 ID 已复制");
  expect(
    calls.find((call) => call.command === "copy_identifier")?.args,
  ).toEqual({ value: firstId });
});

test("settings copy complete identifiers and a failed copy can be retried", async () => {
  let failure = true;
  const { calls, status, poll, page } = await fixture({
    copy_identifier: () => {
      if (failure) throw "CLIPBOARD_ERROR: clipboard unavailable";
      return null;
    },
  });
  status.local.device_id = firstId;
  status.network!.network_id = networkId;
  await poll();
  await page("设置");
  fireEvent.click(document.querySelector("#network-settings > summary")!);
  const copyDevice = screen.getByRole("button", { name: "复制设备 ID" });
  expect(copyDevice.textContent).toBe("9db79d9b…79e1");
  fireEvent.click(copyDevice);
  await screen.findByText("CLIPBOARD_ERROR: clipboard unavailable");
  expect(
    document.querySelector("#network-settings .identifier-error"),
  ).toBeTruthy();
  expect(screen.queryByText("完整 ID 已复制")).toBeNull();
  failure = false;
  fireEvent.click(copyDevice);
  await screen.findByText("完整 ID 已复制");
  expect(
    document.querySelector("#network-settings .identifier-error"),
  ).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "复制网络 ID" }));
  await act(async () => {});
  expect(
    calls
      .filter((call) => call.command === "copy_identifier")
      .map((call) => call.args),
  ).toEqual([{ value: firstId }, { value: firstId }, { value: networkId }]);
});

test("unknown activity sources show a compact ID and remain searchable by the complete ID", async () => {
  const job = commandJob("ABC123", {
    source_device_id: firstId,
    state: "succeeded",
  });
  const { calls, page } = await fixture({
    activity_history: () => ({
      db_id: "db-original",
      entries: [job],
      next_cursor: null,
    }),
    job_output: () => ({ job, has_more: false, events: [] }),
  });
  await page("活动");
  expect((await findActivity("ABC123")).textContent).toContain("9db79d9b…79e1");
  fireEvent.change(screen.getByRole("textbox", { name: "搜索本页活动" }), {
    target: { value: firstId },
  });
  fireEvent.click(await findActivity("ABC123"));
  fireEvent.click(
    document.querySelector(
      "#command-activity-detail .activity-detail-facts > summary",
    )!,
  );
  fireEvent.click(screen.getByRole("button", { name: "复制来源设备 ID" }));
  await screen.findByText("完整 ID 已复制");
  expect(
    calls.find((call) => call.command === "copy_identifier")?.args,
  ).toEqual({ value: firstId });
});
