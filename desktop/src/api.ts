import { invoke } from "@tauri-apps/api/core";
import type { ApiError } from "./errors";
import type { PlainMessageKey } from "./i18n";

export interface LocalStatus {
  joined: boolean;
  device_id: string | null;
  name: string | null;
  version: string;
  daemon_running: boolean;
  daemon_connected: boolean | null;
  remote_access_paused: boolean;
  allow_all: boolean;
  daemon_installed: boolean;
}

export interface Status {
  local: LocalStatus;
  network: {
    network_id: string;
    manager_id: string;
    manager_name: string;
    is_manager: boolean;
    relay_addresses: string[];
  } | null;
  service: {
    installed: boolean;
    approval_required: boolean;
    app_at_login: boolean;
    legacy_installed: boolean;
    development: boolean;
  };
  allow_from: string[];
  deny_from: string[];
  error: ApiError | null;
}

export interface ExecutionSettings {
  default_cwd: string | null;
  max_concurrent_jobs: number;
  path: string | null;
}

export interface Settings {
  execution: ExecutionSettings;
  home_dir: string;
  data_dir: string;
  os: string;
}

export interface Device {
  device_id: string;
  name: string;
  online: boolean;
  revoked: boolean;
  admin: boolean;
  os: string | null;
}

export interface Job {
  job_id: string;
  db_id: string;
  source_device_id: string;
  target_device_id: string;
  program: string;
  args: string[];
  cwd: string;
  state:
    | "starting"
    | "running"
    | "exited"
    | "failed"
    | "canceled"
    | "timed_out"
    | "lost";
  exit_code: number | null;
  signal: number | null;
  duration_ms: number | null;
  last_seq: number;
  output_complete: boolean;
  incomplete_reason: string | null;
  error: string | null;
  created_at_ms: number;
  updated_at_ms: number;
  leftover_possible: boolean;
}

export interface LogEvent {
  seq: number;
  stream: string;
  data_base64: string;
}

export interface TaskOutput {
  job: Job;
  events: LogEvent[];
  has_more: boolean;
}

export interface FileRecord {
  time_ms: number;
  source_device_id: string;
  op: string;
  path: string | null;
  size: number | null;
  result: string;
}

export type TaskFilter = "all" | "running" | "failed";
export type ActionRequest =
  | { command: "start" | "stop" | "remove_service" | "hide_icon" }
  | { command: "join" | "create_network"; args: { link: string; name: string } }
  | { command: "autostart"; args: { enabled: boolean } }
  | { command: "permission"; args: { device: string; allow: boolean } }
  | { command: "all_permissions"; args: { allow: boolean } }
  | { command: "pause_access"; args: { paused: boolean } }
  | { command: "save_settings"; args: { execution: ExecutionSettings } };
export type Action = (request: ActionRequest) => Promise<boolean>;
export interface ConfirmOptions {
  label: string;
  tone?: "danger";
}
export type Confirm = (
  title: string,
  message: string,
  options: ConfirmOptions,
) => Promise<boolean>;
export type PendingOperation =
  ActionRequest["command"] | "invite" | "copy_invitation" | "revoke";
export type Operation = <T>(
  operation: () => Promise<T>,
  context: { name: PendingOperation; title: PlainMessageKey },
) => Promise<T | undefined>;

export interface Invitation {
  link: string;
  allow: boolean;
  expires_in: number;
}

export interface Revocation {
  device_id: string;
  revoked: boolean;
  roster_version: number;
  undelivered: string[];
  sync_error: string | null;
}

export const api = {
  windowVisible: () => invoke<boolean>("window_visible"),
  status: () => invoke<Status>("status"),
  settings: () => invoke<Settings>("settings"),
  invite: (allow: boolean) => invoke<Invitation>("invite", { allow }),
  revoke: (device: string) => invoke<Revocation>("revoke", { device }),
  copyInvitation: (link: string) => invoke<void>("copy_invitation", { link }),
  devices: () =>
    invoke<{
      devices: Device[] | null;
      server_error: { code: string; message: string } | null;
    }>("devices"),
  chooseDirectory: () => invoke<string | null>("choose_directory"),
  action: (request: ActionRequest) =>
    invoke<void>(request.command, "args" in request ? request.args : {}),
  tasks: (before: number | null, filter: TaskFilter) =>
    invoke<{ db_id: string | null; jobs: Job[]; next_cursor: number | null }>(
      "task_history",
      { before, filter },
    ),
  files: (before: number | null) =>
    invoke<{ entries: FileRecord[]; next_cursor: number | null }>(
      "file_history",
      { before },
    ),
  output: (dbId: string, job: string, after: number | null) =>
    invoke<TaskOutput>("task_output", { dbId, job, after }),
};
