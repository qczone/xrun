import { expect, spyOn } from "bun:test";
import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { mockIPC } from "@tauri-apps/api/mocks";
import { StrictMode } from "react";
import { App } from "../src/App";
import type {
  Job,
  CommandJob,
  JobCommon,
  Attachment,
  Device,
  Settings,
  Status,
} from "../src/api";
import {
  initializeLanguage,
  resolveLanguage,
  t,
  type LanguagePreference,
} from "../src/i18n";
type Args = Record<string, unknown>;

type Handler = (args: Args) => unknown;

export async function fixture(
  handlers: Record<string, Handler> = {},
  joined = true,
  locale = "zh-CN",
) {
  const jobs = new Map<string, Job>();
  const remember = (value: unknown) => {
    const page = value as { entries?: Job[] };
    for (const job of page?.entries || []) jobs.set(job.job_id, job);
    return value;
  };
  const calls: { command: string; args: Args }[] = [];
  let languageSettings = {
    preference: "system" as LanguagePreference,
    language: resolveLanguage(locale),
  };
  const status: Status = {
    local: {
      joined,
      device_id: "self",
      name: "mac1",
      version: "0.1.0-rc.4",
      daemon_running: true,
      daemon_connected: true,
      remote_access_paused: false,
      allow_all: false,
      daemon_installed: true,
    },
    network: joined
      ? {
          network_id: "net-test",
          manager_id: "manager",
          manager_name: "manager1",
          is_manager: false,
          relay_addresses: ["https://relay.example:8080"],
        }
      : null,
    service: {
      app_at_login: false,
      installed: true,
      legacy_installed: false,
      approval_required: false,
      development: false,
    },
    allow_from: [],
    deny_from: [],
    error: null,
  };
  const settings: Settings = {
    execution: { default_cwd: null, max_concurrent_jobs: 4, path: null },
    attachment_retention_days: 30,
    home_dir: "/Users/test",
    data_dir: "/Users/test/.xrun",
    os: "macos",
  };
  const devices: Device[] = [
    {
      device_id: "unsafe-label",
      name: '<img id="injected" src=x onerror=alert(1)>',
      online: true,
      revoked: false,
      admin: false,
      os: "windows",
    },
    {
      device_id: "revoked",
      name: "old-device",
      online: false,
      revoked: true,
      admin: false,
      os: "linux",
    },
  ];
  const timers = new Map<number, () => void>();
  let timerId = 0;
  const realSetInterval = globalThis.setInterval;
  const realClearInterval = globalThis.clearInterval;
  // Control application polling without swallowing waitFor's retry timers.
  spyOn(globalThis, "setInterval").mockImplementation(((
    callback: () => void,
    delay?: number,
  ) => {
    if (delay !== 3000) return realSetInterval(callback, delay);
    timers.set(--timerId, callback);
    return timerId;
  }) as unknown as typeof setInterval);
  spyOn(globalThis, "clearInterval").mockImplementation((id) => {
    if (!timers.delete(Number(id))) realClearInterval(Number(id));
  });
  mockIPC(
    (command, payload) => {
      const args = (payload || {}) as Args;
      calls.push({ command, args });
      if (handlers[command]) {
        const value = handlers[command](args);
        return command === "activity_history"
          ? Promise.resolve(value).then(remember)
          : value;
      }
      switch (command) {
        case "window_visible":
          return true;
        case "language_settings":
          return structuredClone(languageSettings);
        case "set_language": {
          const preference = args.preference as LanguagePreference;
          languageSettings = {
            preference,
            language:
              preference === "system" ? resolveLanguage(locale) : preference,
          };
          return structuredClone(languageSettings);
        }
        case "status":
          return structuredClone(status);
        case "settings":
          return structuredClone(settings);
        case "devices":
          return { devices, server_error: null };
        case "save_settings":
          settings.execution = args.execution as Settings["execution"];
          return null;
        case "save_attachment_retention":
          settings.attachment_retention_days = args.days as number;
          return null;
        case "join":
          status.local.joined = true;
          return null;
        case "permission":
          status.allow_from = args.allow ? [args.device as string] : [];
          status.deny_from = args.allow ? [] : [args.device as string];
          return null;
        case "all_permissions":
          status.local.allow_all = args.allow as boolean;
          return null;
        case "pause_access":
          status.local.remote_access_paused = args.paused as boolean;
          return null;
        case "autostart":
          status.service.app_at_login = args.enabled as boolean;
          return null;
        case "activity_job":
          return structuredClone(jobs.get(args.id as string));
        case "activity_history":
          return { db_id: null, entries: [], next_cursor: null };
        default:
          return null;
      }
    },
    { shouldMockEvents: true },
  );
  Object.defineProperty(globalThis, "isTauri", {
    configurable: true,
    value: true,
  });
  await initializeLanguage();
  const view = render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
  await waitFor(() =>
    expect(
      screen
        .getByLabelText(t("settings.defaultCwd"))
        .getAttribute("placeholder"),
    ).toBe("/Users/test"),
  );
  await act(async () => {});
  const poll = async () => {
    await act(async () => {
      for (const callback of [...timers.values()]) callback();
    });
  };
  const page = (name: string) =>
    fireEvent.click(screen.getByRole("button", { name }));
  return { ...view, calls, status, settings, devices, poll, page };
}

export function openMemberActions(name: string) {
  const summary = screen.getByLabelText(`${name} 的更多操作`);
  if (!summary.closest("details")!.open) fireEvent.click(summary);
}

function common(id: string): JobCommon {
  return {
    job_id: id,
    db_id: "db-original",
    request_id: `request-${id}`,
    request_hash: `hash-${id}`,
    source_device_id: "unsafe-label",
    target_device_id: "self",
    state: "running",
    last_log_seq: 3,
    log_bytes: 0,
    output_complete: null,
    output_loss_reason: null,
    error_code: null,
    error_message: null,
    created_at_ms: Date.now() - 1000,
    started_at_ms: Date.now() - 900,
    finished_at_ms: null,
    updated_at_ms: Date.now(),
    leftover_possible: false,
    process: null,
    attachments: [],
  };
}
export function task(
  id = "ABC123",
  changes: Partial<CommandJob> = {},
): CommandJob {
  return {
    ...common(id),
    kind: "exec",
    output_complete: true,
    params: {
      program: "echo",
      args: ['<img id="command-injected" src=x>'],
      cwd: "/repo",
      timeout: 60,
      shell: null,
      input_size: 0,
      input_sha256: "a".repeat(64),
    },
    result: null,
    ...changes,
  };
}

export const event = (
  seq: number,
  stream: string,
  bytes: string | number[],
) => ({
  seq,
  stream,
  data_base64: Buffer.from(
    typeof bytes === "string" ? bytes : new Uint8Array(bytes),
  ).toString("base64"),
});

export function operation(
  kind: "push" | "pull" | "screenshot" | "stream_exec" | "forward",
  changes: Partial<JobCommon> & {
    path?: string;
    size?: number;
    attachment?: Attachment;
  } = {},
): Job {
  const {
    path = "/repo/report.txt",
    size = 68,
    attachment,
    ...overrides
  } = changes;
  const base = {
    ...common(attachment?.id || kind),
    state: "succeeded" as const,
    finished_at_ms: Date.now(),
    ...overrides,
    attachments: attachment ? [attachment] : overrides.attachments || [],
  };
  const file = { path, size, sha256: "a".repeat(64), attachment_error: null };
  switch (kind) {
    case "push":
      return {
        ...base,
        kind,
        params: {
          path,
          cwd: null,
          size,
          sha256: file.sha256,
          mkdir: false,
          no_overwrite: false,
          expect: null,
        },
        result: file,
      };
    case "pull":
      return { ...base, kind, params: { path, cwd: null }, result: file };
    case "screenshot":
      return {
        ...base,
        kind,
        params: {},
        result: {
          captured_at: new Date().toISOString(),
          width: 1,
          height: 1,
          size,
          sha256: file.sha256,
          attachment_error: null,
        },
      };
    case "forward":
      return {
        ...base,
        kind,
        params: { port: 8080 },
        result: {
          port: 8080,
          duration_ms: 100,
          input_bytes: 10,
          output_bytes: 20,
        },
      };
    case "stream_exec":
      return { ...task(base.job_id, base), kind };
  }
}
