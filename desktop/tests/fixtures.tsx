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
import type { Device, Job, Settings, Status } from "../src/api";
type Args = Record<string, unknown>;

type Handler = (args: Args) => unknown;

export async function fixture(
  handlers: Record<string, Handler> = {},
  joined = true,
) {
  const calls: { command: string; args: Args }[] = [];
  const status: Status = {
    local: {
      joined,
      device_id: "self",
      name: "mac1",
      version: "0.0.1-beta.5",
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
  mockIPC((command, payload) => {
    const args = (payload || {}) as Args;
    calls.push({ command, args });
    if (handlers[command]) return handlers[command](args);
    switch (command) {
      case "status":
        return structuredClone(status);
      case "settings":
        return structuredClone(settings);
      case "devices":
        return { devices, server_error: null };
      case "save_settings":
        settings.execution = args.execution as Settings["execution"];
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
      case "task_history":
        return { db_id: null, jobs: [], next_cursor: null };
      case "file_history":
        return { entries: [], next_cursor: null };
      default:
        return null;
    }
  });
  const view = render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
  await waitFor(() =>
    expect(
      screen.getByLabelText("默认工作目录").getAttribute("placeholder"),
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

export function task(id = "ABC123", changes: Partial<Job> = {}): Job {
  return {
    job_id: id,
    db_id: "db-original",
    source_device_id: "unsafe-label",
    target_device_id: "self",
    program: "echo",
    args: ['<img id="command-injected" src=x>'],
    cwd: "/repo",
    state: "running",
    exit_code: null,
    signal: null,
    duration_ms: null,
    last_seq: 3,
    output_complete: true,
    incomplete_reason: null,
    error: null,
    created_at_ms: Date.now() - 1000,
    updated_at_ms: Date.now(),
    leftover_possible: false,
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
