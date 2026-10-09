import { expect, test } from "bun:test";
import { fireEvent, screen, waitFor } from "@testing-library/react";
import { fixture } from "./fixtures";
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

test("attachment retention can be saved before joining without losing unsaved execution inputs", async () => {
  const { calls, poll, page, settings } = await fixture({}, false);
  page("设置");
  const days = screen.getByLabelText("附件保留时间（天）") as HTMLInputElement;
  expect(days.value).toBe("30");
  expect(days.disabled).toBe(false);
  fireEvent.change(days, { target: { value: "7" } });
  await poll();
  page("本机");
  page("设置");
  expect(days.value).toBe("7");
  fireEvent.submit(document.getElementById("retention-form")!);
  await screen.findByText("附件保留时间已保存");
  expect(
    calls.find((call) => call.command === "save_attachment_retention")?.args,
  ).toEqual({ days: 7 });
  expect(settings.attachment_retention_days).toBe(7);
  expect(calls.some((call) => call.command === "save_settings")).toBe(false);
});
