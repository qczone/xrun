import { afterEach, expect, test } from "bun:test";
import { act, fireEvent, screen, within } from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import {
  initializeLanguage,
  changeLanguage,
  resolveLanguage,
  setLanguage,
  t,
} from "../src/i18n";
import { en } from "../src/i18n/en";
import { zh } from "../src/i18n/zh";
import { clockTime, duration, recordTime } from "../src/format";
import { event, fixture, task } from "./fixtures";

const browserLanguages = Object.getOwnPropertyDescriptor(
  navigator,
  "languages",
);
afterEach(() => {
  if (browserLanguages)
    Object.defineProperty(navigator, "languages", browserLanguages);
  else Reflect.deleteProperty(navigator, "languages");
});

test("Chinese system language variants select Chinese; all others default to English", () => {
  for (const locale of ["zh", "zh-CN", "zh-Hans-CN", "zh-Hant-TW", "ZH_hk"])
    expect(resolveLanguage(locale)).toBe("zh");
  for (const locale of [null, undefined, "", "en-CN", "ja-JP", "zhx"])
    expect(resolveLanguage(locale)).toBe("en");
});

test("both catalogs have complete, nonempty messages with matching placeholders", () => {
  expect(Object.keys(zh).sort()).toEqual(Object.keys(en).sort());
  const placeholders = (message: string) =>
    [...message.matchAll(/\{(\w+)\}/g)].map((match) => match[1]).sort();
  for (const key of Object.keys(en) as (keyof typeof en)[]) {
    expect(zh[key].trim()).not.toBe("");
    expect(en[key].trim()).not.toBe("");
    expect(en[key]).not.toMatch(/\p{Script=Han}/u);
    expect(placeholders(zh[key])).toEqual(placeholders(en[key]));
  }
});

test("native locale takes priority over the webview language and detection errors use English", async () => {
  Object.defineProperty(globalThis, "isTauri", {
    configurable: true,
    value: true,
  });
  Object.defineProperty(navigator, "languages", {
    configurable: true,
    value: ["en-US"],
  });
  mockIPC((command) => {
    expect(command).toBe("language_settings");
    return { preference: "system", language: "zh" };
  });
  await initializeLanguage();
  expect(document.documentElement.lang).toBe("zh-CN");
  expect(t("nav.devices")).toBe("设备");
  mockIPC(() => {
    throw new Error("native detection unavailable");
  });
  await initializeLanguage();
  expect(document.documentElement.lang).toBe("en");
  expect(t("nav.devices")).toBe("Devices");
});

test("browser preview uses only its primary language and does not invoke native IPC", async () => {
  mockIPC(() => {
    throw new Error("preview must not invoke IPC");
  });
  for (const [locales, expected] of [
    [["ja-JP", "zh-CN"], "en"],
    [["zh-TW", "en-US"], "zh-CN"],
  ] as const) {
    Object.defineProperty(navigator, "languages", {
      configurable: true,
      value: locales,
    });
    await initializeLanguage();
    expect(document.documentElement.lang).toBe(expected);
  }
});

test("dates, times and durations follow the selected interface language", () => {
  const time = Date.UTC(2026, 9, 7, 13, 5, 0);
  for (const [language, locale] of [
    ["en", "en-US"],
    ["zh", "zh-CN"],
  ] as const) {
    setLanguage(language);
    expect(recordTime(time)).toBe(
      new Date(time).toLocaleString(locale, { hour12: false }),
    );
    expect(clockTime(time)).toBe(new Date(time).toLocaleTimeString(locale));
  }
  expect(duration(65_000)).toBe("1 分 5 秒");
  setLanguage("en");
  expect(duration(65_000)).toBe("1 min 5 s");
  expect(duration(1500)).toBe("1.5 s");
  expect(duration(25)).toBe("25 ms");
});

test("English UI localizes all pages, dynamic labels, confirmations and operation errors", async () => {
  const name = "<img id=untrusted-device> {name}";
  const job = task("TASK-42", {
    state: "failed",
    result: {
      exit_code: 1,
      signal: null,
      duration_ms: 65_000,
      input_bytes: null,
      stdout_bytes: null,
      stderr_bytes: null,
    },
  });
  const { devices, status, poll, page } = await fixture(
    {
      activity_history: () => ({
        db_id: job.db_id,
        entries: [job],
        next_cursor: null,
      }),
      job_output: () => ({ job, events: [], has_more: false }),
      start: () => {
        throw { code: "HELPER_NOT_FOUND", message: "technical detail" };
      },
    },
    true,
    "en-US",
  );
  expect(screen.getByRole("heading", { name: "Device overview" })).toBeTruthy();
  devices[0].name = name;
  page("Devices");
  await screen.findByLabelText(`Allow ${name} to access this device`);
  expect(screen.getByText("Network members · 1")).toBeTruthy();
  fireEvent.click(screen.getByLabelText(`Allow ${name} to access this device`));
  const confirmation = await screen.findByRole("dialog", {
    name: `Allow ${name} to access this device?`,
  });
  expect(document.querySelector("#untrusted-device")).toBeNull();
  expect(confirmation.textContent).not.toMatch(/\p{Script=Han}/u);
  expect(
    within(confirmation).getByRole("button", { name: "Cancel" }),
  ).toBeTruthy();
  await act(async () => (confirmation as HTMLDialogElement).close("cancel"));
  page("Activity journey");
  fireEvent.click(await screen.findByRole("button", { name: /TASK-42/ }));
  await screen.findByText("Task finished");
  expect(screen.getByText("Exit code 1")).toBeTruthy();
  expect(
    screen.getByText("Duration: 1 min 5 s", {
      selector: ".task-summary-meta span",
    }),
  ).toBeTruthy();
  page("Settings");
  expect(screen.getByLabelText("Default working directory")).toBeTruthy();
  expect(screen.getByText("Tool search path (PATH)")).toBeTruthy();
  status.local.daemon_running = false;
  await poll();
  page("This device");
  fireEvent.click(
    screen.getByRole("button", { name: "Start background service" }),
  );
  const alert = await screen.findByRole("alert");
  expect(alert.textContent).toContain("Could not start the background service");
  expect(alert.textContent).toContain("HELPER_NOT_FOUND: technical detail");
  expect(
    document.querySelector("main")?.textContent?.replaceAll("中文", ""),
  ).not.toMatch(/\p{Script=Han}/u);
});

test("language switching is immediate, persists, and preserves unsaved fields and selected activity", async () => {
  const job = task("LANG-42", {
    state: "succeeded",
    result: {
      exit_code: 0,
      signal: null,
      duration_ms: 1000,
      input_bytes: null,
      stdout_bytes: null,
      stderr_bytes: null,
    },
  });
  const { page, calls } = await fixture(
    {
      devices: () => ({ devices: [], server_error: null }),
      activity_history: () => ({
        db_id: job.db_id,
        entries: [job],
        next_cursor: null,
      }),
      job_output: () => ({
        job,
        events: [event(1, "stdout", "original output")],
        has_more: false,
      }),
    },
    true,
    "en-US",
  );
  page("Activity journey");
  fireEvent.click(await screen.findByRole("button", { name: /LANG-42/ }));
  await screen.findByText("Task finished");
  page("Settings");
  fireEvent.change(screen.getByLabelText("Default working directory"), {
    target: { value: "/unsaved/path" },
  });
  fireEvent.change(screen.getByLabelText("Tool search path"), {
    target: { value: "/custom/bin" },
  });
  fireEvent.change(screen.getByLabelText("Language"), {
    target: { value: "zh" },
  });
  const chinese = await screen.findByLabelText("语言");
  expect((chinese as HTMLSelectElement).value).toBe("zh");
  expect(document.documentElement.lang).toBe("zh-CN");
  expect(
    (screen.getByLabelText("默认工作目录") as HTMLInputElement).value,
  ).toBe("/unsaved/path");
  expect(
    (screen.getByLabelText("工具搜索路径") as HTMLTextAreaElement).value,
  ).toBe("/custom/bin");
  expect(screen.getByRole("button", { name: "活动旅程" })).toBeTruthy();
  expect(screen.queryByRole("button", { name: "活动记录" })).toBeNull();
  page("活动旅程");
  expect(screen.getByText("任务已结束")).toBeTruthy();
  expect(screen.getByText("original output")).toBeTruthy();
  page("设备");
  expect(
    screen.getByText(
      "暂时没有其他设备。新设备需要管理设备生成的邀请链接才能加入。",
    ),
  ).toBeTruthy();
  page("设置");
  fireEvent.change(screen.getByLabelText("语言"), {
    target: { value: "system" },
  });
  const automatic = await screen.findByLabelText("Language");
  expect((automatic as HTMLSelectElement).value).toBe("system");
  expect(
    (screen.getByLabelText("Default working directory") as HTMLInputElement)
      .value,
  ).toBe("/unsaved/path");
  fireEvent.change(automatic, { target: { value: "zh" } });
  await screen.findByLabelText("语言");
  await act(async () => {
    await initializeLanguage();
  });
  expect((screen.getByLabelText("语言") as HTMLSelectElement).value).toBe("zh");
  expect(
    calls
      .filter((call) => call.command === "set_language")
      .map((call) => call.args.preference),
  ).toEqual(["zh", "system", "zh"]);
});

test("failed language saves keep the previous preference and can be retried", async () => {
  let rejectSave!: (reason: unknown) => void;
  let attempts = 0;
  const pending = new Promise((_, reject) => {
    rejectSave = reject;
  });
  const { page } = await fixture(
    {
      set_language: () =>
        ++attempts === 1 ? pending : { preference: "zh", language: "zh" },
    },
    true,
    "en-US",
  );
  page("Settings");
  const select = screen.getByLabelText("Language") as HTMLSelectElement;
  fireEvent.change(select, { target: { value: "zh" } });
  expect(select.disabled).toBe(true);
  expect(select.value).toBe("system");
  await act(async () => {
    rejectSave({ code: "STORAGE_ERROR", message: "disk full" });
  });
  expect((await screen.findByRole("alert")).textContent).toContain(
    "Could not save the language",
  );
  expect(select.disabled).toBe(false);
  expect(document.documentElement.lang).toBe("en");
  fireEvent.change(select, { target: { value: "zh" } });
  await screen.findByLabelText("语言");
  expect(screen.queryByRole("alert")).toBeNull();
});

test("browser preview restores manual choices and resumes following the system", async () => {
  Object.defineProperty(navigator, "languages", {
    configurable: true,
    value: ["zh-CN"],
  });
  await initializeLanguage();
  expect(document.documentElement.lang).toBe("zh-CN");
  await changeLanguage("en");
  await initializeLanguage();
  expect(document.documentElement.lang).toBe("en");
  expect(window.localStorage.getItem("xrun.language")).toBe("en");
  await changeLanguage("system");
  expect(document.documentElement.lang).toBe("zh-CN");
  window.localStorage.setItem("xrun.language", "unsupported");
  await initializeLanguage();
  expect(document.documentElement.lang).toBe("zh-CN");
});

test("Chinese system languages render the Chinese initial UI", async () => {
  await fixture({}, false, "zh-Hant-TW");
  expect(screen.getByRole("heading", { name: "连接你的设备" })).toBeTruthy();
});

test("unsupported system languages render the English initial UI", async () => {
  await fixture({}, false, "fr-FR");
  expect(
    screen.getByRole("heading", { name: "Connect your devices" }),
  ).toBeTruthy();
  expect(screen.getByLabelText("Device name")).toBeTruthy();
  expect(document.documentElement.lang).toBe("en");
});
