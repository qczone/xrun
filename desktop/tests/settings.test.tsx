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
