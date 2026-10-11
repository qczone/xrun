import { expect, test } from "bun:test";
import { act, fireEvent, screen, waitFor } from "@testing-library/react";
import { emit } from "@tauri-apps/api/event";
import type { DesktopNavigation } from "../src/api";
import { fixture } from "./fixtures";

const selectedPage = () =>
  document
    .querySelector('nav [aria-current="page"]')
    ?.getAttribute("data-page");

test("a tray request made before the WebView is ready opens the requested page", async () => {
  await fixture({
    desktop_navigation: () => ({ revision: 1, page: "activity" }),
  });
  await waitFor(() => expect(selectedPage()).toBe("activity"));
  expect(screen.getByRole("heading", { name: "活动" })).toBeTruthy();
});

test("tray shortcuts navigate while hidden and retain unsaved settings", async () => {
  const { calls, page, poll } = await fixture();
  await page("设置");
  fireEvent.click(screen.getByText("执行环境", { selector: "summary span" }));
  fireEvent.change(screen.getByLabelText("默认工作目录"), {
    target: { value: "/unsaved-project" },
  });
  await act(async () => {
    await emit("xrun-window-visible", false);
    await emit("xrun-desktop-navigation", { revision: 1, page: "activity" });
  });
  expect(selectedPage()).toBe("activity");
  const activityReads = calls.filter(
    (call) => call.command === "activity_history",
  ).length;
  await poll();
  expect(
    calls.filter((call) => call.command === "activity_history").length,
  ).toBe(activityReads);
  await act(async () => {
    await emit("xrun-window-visible", true);
  });
  await waitFor(() =>
    expect(
      calls.filter((call) => call.command === "activity_history").length,
    ).toBeGreaterThan(activityReads),
  );
  await act(async () => {
    await emit("xrun-desktop-navigation", { revision: 2, page: "settings" });
  });
  expect(selectedPage()).toBe("settings");
  expect(
    (screen.getByLabelText("默认工作目录") as HTMLInputElement).value,
  ).toBe("/unsaved-project");
});

test("a late startup response and older events cannot override the latest tray click", async () => {
  let complete!: (request: DesktopNavigation) => void;
  const pending = new Promise<DesktopNavigation>((resolve) => {
    complete = resolve;
  });
  await fixture({ desktop_navigation: () => pending });
  await act(async () => {
    await emit("xrun-desktop-navigation", { revision: 2, page: "settings" });
    complete({ revision: 1, page: "activity" });
  });
  expect(selectedPage()).toBe("settings");
  await act(async () => {
    await emit("xrun-desktop-navigation", { revision: 1, page: "devices" });
  });
  expect(selectedPage()).toBe("settings");
});
