import { expect, test } from "bun:test";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { fixture } from "./fixtures";
test("settings save only execution fields and preserve unsaved input across polling and navigation", async () => {
  const { calls, poll, page } = await fixture();
  await page("设置");
  fireEvent.click(screen.getByText("执行环境", { selector: "summary span" }));
  const input = screen.getByLabelText("同时运行的任务数") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "8" } });
  await poll();
  await page("设备");
  await page("设置");
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

test("attachment retention can be saved before joining without losing unsaved execution inputs", async () => {
  const { calls, poll, page, settings } = await fixture({}, false);
  await page("设置");
  const days = screen.getByLabelText("附件保留时间（天）") as HTMLInputElement;
  expect(days.value).toBe("30");
  expect(days.disabled).toBe(false);
  fireEvent.change(days, { target: { value: "7" } });
  await poll();
  await page("设备");
  await page("设置");
  expect(days.value).toBe("7");
  fireEvent.submit(document.getElementById("retention-form")!);
  await screen.findByText("附件保留时间已保存");
  expect(
    calls.find((call) => call.command === "save_attachment_retention")?.args,
  ).toEqual({ days: 7 });
  expect(settings.attachment_retention_days).toBe(7);
  expect(calls.some((call) => call.command === "save_settings")).toBe(false);
});

test("membership freshness is editable only by the manager", async () => {
  const { calls, poll, page, status, settings } = await fixture();
  await page("设置");
  expect(screen.queryByRole("combobox", { name: "成员有效期" })).toBeNull();
  if (status.network) status.network.is_manager = true;
  await poll();
  fireEvent.click(screen.getByText("网络与服务", { selector: "summary span" }));
  const control = await screen.findByRole("combobox", { name: "成员有效期" });
  fireEvent.keyDown(control, { key: "ArrowDown" });
  const option = await screen.findByRole("option", { name: "60 分钟" });
  fireEvent.click(option);
  await waitFor(() => expect(settings.membership_lease_minutes).toBe(60));
  expect(
    calls.find((call) => call.command === "save_membership_lease")?.args,
  ).toEqual({ minutes: 60 });
});

test("bandwidth policy has independent local controls", async () => {
  const { calls, page, settings } = await fixture();
  await page("设置");
  const control = await screen.findByRole("combobox", { name: "省流量" });
  fireEvent.keyDown(control, { key: "ArrowDown" });
  fireEvent.click(await screen.findByRole("option", { name: "关闭" }));
  await waitFor(() => expect(settings.traffic_compression).toBe("none"));
  expect(
    calls.find((call) => call.command === "save_traffic_compression")?.args,
  ).toEqual({ compression: "none" });
});

test("direct networking is opt in and selects one private interface", async () => {
  const { calls, page, settings } = await fixture();
  await page("设置");
  const control = await screen.findByRole("combobox", { name: "局域网直连" });
  expect(settings.lan_address).toBeNull();
  fireEvent.keyDown(control, { key: "ArrowDown" });
  fireEvent.click(
    await screen.findByRole("option", { name: "192.168.5.10 · en0" }),
  );
  await waitFor(() => expect(settings.lan_address).toBe("192.168.5.10"));
  expect(
    calls.find((call) => call.command === "save_direct_connection")?.args,
  ).toEqual({ address: "192.168.5.10" });
});
