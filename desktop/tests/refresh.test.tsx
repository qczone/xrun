import { expect, test } from "bun:test";
import {
  act,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { commandJob, findActivity, fixture } from "./fixtures";
import type { TrafficReport } from "../src/api";
import { t } from "../src/i18n";

test("empty devices stay readable during polling and slow requests do not overlap", async () => {
  let delay = false;
  let complete: ((value: unknown) => void) | undefined;
  const result = { devices: [], server_error: null };
  const { poll, calls } = await fixture({
    devices: () =>
      delay ? new Promise((resolve) => (complete = resolve)) : result,
  });
  const page = document.getElementById("page-devices")!;
  const refresh = within(page).getByRole("button", { name: "刷新" });
  await screen.findByText(t("devices.empty"));
  refresh.focus();
  delay = true;
  await poll();
  expect(screen.getByText(t("devices.empty"))).toBeTruthy();
  expect(screen.queryByText(t("devices.reading"))).toBeNull();
  expect(refresh.getAttribute("aria-label")).toBe("刷新");
  expect(refresh.getAttribute("aria-busy")).toBe("true");
  expect(document.activeElement).toBe(refresh);
  const count = calls.filter((call) => call.command === "devices").length;
  fireEvent.click(refresh);
  await poll();
  expect(calls.filter((call) => call.command === "devices")).toHaveLength(
    count,
  );
  await act(async () => complete?.(result));
  await waitFor(() =>
    expect((refresh as HTMLButtonElement).disabled).toBe(false),
  );
});

test("activity pagination remains usable during polling and ignores its late response", async () => {
  let delay = false;
  let complete: ((value: unknown) => void) | undefined;
  const result = {
    db_id: "db-original",
    entries: [commandJob("PAGE1")],
    next_cursor: "page-2",
  };
  const { page, poll } = await fixture({
    activity_history: ({ before }) =>
      before
        ? { ...result, entries: [commandJob("PAGE2")], next_cursor: null }
        : delay
          ? new Promise((resolve) => (complete = resolve))
          : result,
  });
  await page("活动");
  const first = await findActivity("PAGE1");
  const refresh = screen.getByRole("button", { name: "刷新" });
  const next = screen.getByRole("button", { name: "下一页" });
  delay = true;
  await poll();
  expect(await findActivity("PAGE1")).toBe(first);
  expect(refresh.textContent).toBe("刷新");
  expect(refresh.getAttribute("aria-busy")).toBe("true");
  expect((next as HTMLButtonElement).disabled).toBe(false);
  fireEvent.click(next);
  await findActivity("PAGE2");
  await act(async () => complete?.(result));
  expect(await findActivity("PAGE2")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /PAGE1/ })).toBeNull();
});

test("an empty activity timeline stays empty while it updates", async () => {
  let delay = false;
  let complete: ((value: unknown) => void) | undefined;
  const result = { db_id: null, entries: [], next_cursor: null };
  const { page, poll } = await fixture({
    activity_history: () =>
      delay ? new Promise((resolve) => (complete = resolve)) : result,
  });
  await page("活动");
  await screen.findByText(t("activity.empty"));
  delay = true;
  await poll();
  expect(screen.getByText(t("activity.empty"))).toBeTruthy();
  expect(screen.queryByText(t("activity.loading"))).toBeNull();
  await act(async () => complete?.(result));
});

test("traffic retains totals and its chart during a slow automatic update", async () => {
  let delay = false;
  let complete: ((value: TrafficReport) => void) | undefined;
  const result = (period: TrafficReport["period"]): TrafficReport => ({
    network_id: "net-test",
    device_id: "self",
    period,
    start_ms: 0,
    end_ms: Date.now(),
    recorded_since_ms: Date.now(),
    complete: true,
    totals: { ingress_bytes: 1024, egress_bytes: 2048 },
    devices: [],
    daily: [{ start_ms: Date.now(), ingress_bytes: 1024, egress_bytes: 2048 }],
    next_offset: null,
  });
  const { poll } = await fixture({
    traffic: ({ query }) =>
      delay
        ? new Promise<TrafficReport>((resolve) => (complete = resolve))
        : result((query as { period: TrafficReport["period"] }).period),
  });
  fireEvent.click(screen.getByRole("button", { name: "详情" }));
  const dialog = screen.getByRole("dialog", { name: "流量统计" });
  await within(dialog).findByText("1.0 KiB");
  const chart = within(dialog).getByRole("img", {
    name: t("traffic.dailyTrend"),
  });
  fireEvent.click(within(dialog).getByRole("button", { name: "本月" }));
  expect(within(dialog).getByText("1.0 KiB")).toBeTruthy();
  expect(within(dialog).getByRole("img")).toBe(chart);
  delay = true;
  await poll();
  expect(within(dialog).getByText("1.0 KiB")).toBeTruthy();
  expect(within(dialog).getByRole("img")).toBe(chart);
  expect(within(dialog).queryByText(t("traffic.loading"))).toBeNull();
  expect(
    within(dialog)
      .getByRole("button", { name: "刷新" })
      .getAttribute("aria-busy"),
  ).toBe("true");
  await act(async () => complete?.(result("month")));
});
