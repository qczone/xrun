import { expect, test } from "bun:test";
import {
  act,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { fixture } from "./fixtures";
import type { TrafficReport } from "../src/api";

const report = (
  period: TrafficReport["period"] = "month",
  manager = false,
): TrafficReport => ({
  network_id: "net-test",
  device_id: manager ? null : "self",
  period,
  start_ms: 0,
  end_ms: Date.now(),
  recorded_since_ms: Date.now(),
  complete: true,
  totals: { ingress_bytes: 1024, egress_bytes: 2048 },
  devices: [{ device_id: "self", sent_bytes: 1024, received_bytes: 2048 }],
  daily: [
    {
      start_ms: Date.UTC(2026, 9, 10),
      ingress_bytes: 1024,
      egress_bytes: 2048,
    },
  ],
  next_offset: null,
});

test("traffic shows device send/receive, switches UTC periods, and stops polling while hidden", async () => {
  const { page, calls, poll } = await fixture({
    traffic: ({ query }) =>
      report((query as { period: TrafficReport["period"] }).period),
  });
  const panel = await screen.findByRole("region", { name: "流量统计" });
  await within(panel).findByText("1.0 KiB");
  expect(within(panel).getAllByText("接收").length).toBeGreaterThan(0);
  fireEvent.click(within(panel).getByRole("button", { name: "今日" }));
  await waitFor(() =>
    expect(
      calls.filter((call) => call.command === "traffic").at(-1)?.args.query,
    ).toMatchObject({ period: "today" }),
  );
  page("设备");
  const previous = calls.filter((call) => call.command === "traffic").length;
  await poll();
  expect(calls.filter((call) => call.command === "traffic").length).toBe(
    previous,
  );
});

test("network traffic keeps the last snapshot on failure and offers retry", async () => {
  let fail = false;
  const { status, poll } = await fixture({
    traffic: ({ query }) => {
      if (fail) throw { code: "CONNECT_FAILED", message: "relay offline" };
      return report(
        (query as { period: TrafficReport["period"] }).period,
        true,
      );
    },
  });
  status.network!.is_manager = true;
  await poll();
  const panel = await screen.findByRole("region", { name: "流量统计" });
  await within(panel).findAllByText("中转出站");
  await within(panel).findAllByText("2.0 KiB");
  fail = true;
  await poll();
  await within(panel).findByText("流量统计未能读取，请重试。");
  expect(within(panel).getAllByText("2.0 KiB").length).toBeGreaterThan(0);
  fail = false;
  fireEvent.click(within(panel).getByRole("button", { name: "重试" }));
  await waitFor(() =>
    expect(within(panel).queryByText("流量统计未能读取，请重试。")).toBeNull(),
  );
});

test("a late traffic response cannot restore a previous network or period", async () => {
  let resolve: ((value: TrafficReport) => void) | undefined;
  const { status, poll } = await fixture({
    traffic: () =>
      new Promise<TrafficReport>((done) => {
        resolve = done;
      }),
  });
  status.network!.network_id = "net-new";
  await poll();
  await act(async () => resolve?.(report()));
  expect(screen.queryByText("1.0 KiB")).toBeNull();
});
