import { expect, test } from "bun:test";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import { fixture, selectOption, openMemberActions } from "./fixtures";

test("desktop screenshot presets and custom formats send semantic options and show the saved image", async () => {
  const { calls, devices } = await fixture({
    choose_screenshot_destination: () => "/tmp/owned-fixture-screen.webp",
    remote_screenshot: () => ({
      path: "/tmp/owned-fixture-screen.webp",
      width: 800,
      height: 450,
      format: "webp",
      mime: "image/webp",
      size: 1024,
      sha256: "a".repeat(64),
      preview: "data:image/webp;base64,UklGRg==",
      captured_at: new Date().toISOString(),
    }),
  });
  openMemberActions(devices[0].name);
  const menu = screen
    .getByLabelText(`${devices[0].name} 的更多操作`)
    .closest("details")!;
  fireEvent.click(within(menu).getByRole("button", { name: "截图" }));
  await screen.findByRole("dialog", { name: `截图 · ${devices[0].name}` });
  const preset = screen.getByRole("combobox", { name: "画面大小" });
  await selectOption(preset, "compact");
  fireEvent.submit(
    screen
      .getByRole("button", { name: "截图并保存" })
      .closest("form") as HTMLFormElement,
  );
  await screen.findByText("截图已保存");
  expect(
    calls.find((call) => call.command === "remote_screenshot")?.args.options,
  ).toEqual({ format: "jpeg", max_width: 1600, quality: 80 });
  await selectOption(preset, "custom");
  fireEvent.change(screen.getByLabelText("最大宽度"), {
    target: { value: "800" },
  });
  fireEvent.change(screen.getByLabelText("画质"), { target: { value: "70" } });
  fireEvent.submit(
    screen
      .getByRole("button", { name: "截图并保存" })
      .closest("form") as HTMLFormElement,
  );
  await waitFor(() =>
    expect(
      calls.filter((call) => call.command === "remote_screenshot"),
    ).toHaveLength(2),
  );
  expect(
    calls.filter((call) => call.command === "remote_screenshot")[1].args
      .options,
  ).toEqual({ format: "webp", max_width: 800, quality: 70 });
  expect(screen.getByRole("img", { name: "下载的截图" })).toBeTruthy();
});

test("cancelling the native save dialog does not request a screenshot", async () => {
  const { calls, devices } = await fixture({
    choose_screenshot_destination: () => null,
  });
  openMemberActions(devices[0].name);
  const menu = screen
    .getByLabelText(`${devices[0].name} 的更多操作`)
    .closest("details")!;
  fireEvent.click(within(menu).getByRole("button", { name: "截图" }));
  await screen.findByRole("dialog", { name: `截图 · ${devices[0].name}` });
  fireEvent.submit(
    screen
      .getByRole("button", { name: "截图并保存" })
      .closest("form") as HTMLFormElement,
  );
  await waitFor(() =>
    expect(
      calls.some((call) => call.command === "choose_screenshot_destination"),
    ).toBe(true),
  );
  expect(calls.some((call) => call.command === "remote_screenshot")).toBe(
    false,
  );
});
