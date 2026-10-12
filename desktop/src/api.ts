import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ApiError } from "./errors";
import type { PlainMessageKey } from "./i18n";

export type DesktopPage = "devices" | "activity" | "settings";
export interface DesktopNavigation {
  revision: number;
  page: DesktopPage;
}

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
  attachment_retention_days: number;
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
  arch?: string | null;
  daemon_version?: string | null;
  last_seen?: number | null;
}

export type TrafficPeriod = "today" | "month" | "all";
export interface TrafficReport {
  network_id: string;
  device_id: string | null;
  period: TrafficPeriod;
  start_ms: number;
  end_ms: number;
  recorded_since_ms: number | null;
  complete: boolean;
  totals: { ingress_bytes: number; egress_bytes: number };
  devices: { device_id: string; sent_bytes: number; received_bytes: number }[];
  daily: { start_ms: number; ingress_bytes: number; egress_bytes: number }[];
  next_offset: number | null;
}

export interface JobCommon {
  job_id: string;
  db_id: string;
  request_id: string;
  request_hash: string;
  source_device_id: string;
  target_device_id: string;
  state:
    | "accepted"
    | "running"
    | "succeeded"
    | "failed"
    | "canceled"
    | "timed_out"
    | "lost";
  last_log_seq: number;
  log_bytes: number;
  output_complete: boolean | null;
  output_loss_reason: string | null;
  error_code: string | null;
  error_message: string | null;
  created_at_ms: number;
  started_at_ms: number | null;
  finished_at_ms: number | null;
  updated_at_ms: number;
  leftover_possible: boolean;
  process: { pid: number; boot_id: string; start: string | null } | null;
  attachments: Attachment[];
}
export interface CommandParams {
  program: string;
  args: string[];
  cwd: string;
  timeout: number;
  shell: string | null;
  input_size: number | null;
  input_sha256: string | null;
}
export interface CommandResult {
  exit_code: number | null;
  signal: number | null;
  duration_ms: number;
  input_bytes: number | null;
  stdout_bytes: number | null;
  stderr_bytes: number | null;
}
export type CommandJob = JobCommon & {
  kind: "exec" | "stream_exec";
  params: CommandParams;
  result: CommandResult | null;
};
export interface FileResult {
  path: string;
  size: number;
  sha256: string;
  attachment_error: string | null;
}
export interface PullParams {
  path: string;
  cwd: string | null;
}
export interface PushParams extends PullParams {
  size: number;
  sha256: string;
  mkdir: boolean;
  no_overwrite: boolean;
  expect: string | null;
}
export type FileJob = JobCommon &
  (
    | { kind: "push"; params: PushParams; result: FileResult | null }
    | { kind: "pull"; params: PullParams; result: FileResult | null }
  );
export type ScreenshotJob = JobCommon & {
  kind: "screenshot";
  params: Record<string, never>;
  result: {
    captured_at: string;
    width: number;
    height: number;
    size: number;
    sha256: string;
    attachment_error: string | null;
  } | null;
};
export type ForwardJob = JobCommon & {
  kind: "forward";
  params: { port: number };
  result: {
    port: number;
    duration_ms: number;
    input_bytes: number;
    output_bytes: number;
  } | null;
};
export type Job = CommandJob | FileJob | ScreenshotJob | ForwardJob;

export interface LogEvent {
  seq: number;
  stream: string;
  data_base64: string;
}

export interface JobOutput {
  job: CommandJob;
  events: LogEvent[];
  has_more: boolean;
}

export interface Attachment {
  id: string;
  name: string;
  size: number;
  sha256: string;
  created_at_ms: number;
  status: "available" | "expired" | "missing";
  expires_at_ms: number | null;
}

export interface AttachmentPreview {
  attachment: Attachment;
  image: string | null;
  text: string | null;
}

export type ActivityFilter = "all" | "running" | "failed";
export type ActionRequest =
  | { command: "start" | "stop" | "remove_service" | "hide_icon" }
  | { command: "join" | "create_network"; args: { link: string; name: string } }
  | {
      command: "connect";
      args: { link: string; name: string; allowManager: boolean };
    }
  | { command: "autostart"; args: { enabled: boolean } }
  | { command: "permission"; args: { device: string; allow: boolean } }
  | { command: "all_permissions"; args: { allow: boolean } }
  | { command: "pause_access"; args: { paused: boolean } }
  | { command: "save_attachment_retention"; args: { days: number } }
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
  | ActionRequest["command"]
  | "invite"
  | "copy_invitation"
  | "copy_identifier"
  | "revoke"
  | "cloudflare_accounts"
  | "logout_cloudflare"
  | "deploy_cloudflare"
  | "remote_submit"
  | "remote_cancel";

export interface CloudflareAccount {
  id: string;
  name: string;
}
export interface RelayDeployment {
  provider: "cloudflare";
  name: string;
  account_id: string;
  link: string;
}
export interface RemoteCommand {
  request_id: string;
  script: string;
  shell: string;
  cwd: string | null;
  timeout: number;
}
export interface RemoteReceipt {
  request_id: string;
  db_id: string;
  job_id: string | null;
  status: string;
  created_at_ms: number;
}
export type Operation = <T>(
  operation: () => Promise<T>,
  context: { name: PendingOperation; title: PlainMessageKey },
) => Promise<T | undefined>;

export interface Invitation {
  link: string;
  allow: boolean;
  expires_in: number;
  invitation_id?: string;
  address?: string;
  code?: string;
}

export interface Revocation {
  device_id: string;
  revoked: boolean;
  roster_version: number;
  undelivered: string[];
  sync_error: string | null;
}

export const api = {
  connectionDefaults: () => invoke<string>("connection_defaults"),
  cloudflareAccounts: async (
    onAuthorization: () => void,
    reauthorize = false,
    remember = false,
  ) => {
    const unlisten = await listen<string>(
      "xrun-cloudflare-authorization",
      onAuthorization,
    );
    try {
      return await invoke<CloudflareAccount[] | null>("cloudflare_accounts", {
        reauthorize,
        remember,
      });
    } finally {
      unlisten();
    }
  },
  cancelCloudflareAuthorization: () =>
    invoke<void>("cancel_cloudflare_authorization"),
  openCloudflareAuthorization: () =>
    invoke<void>("open_cloudflare_authorization"),
  deployCloudflare: (name: string, account: string) =>
    invoke<RelayDeployment>("deploy_cloudflare", { name, account }),
  cloudflareRelays: () => invoke<RelayDeployment[]>("cloudflare_relays"),
  cloudflareRememberedLogin: () =>
    invoke<boolean>("cloudflare_remembered_login"),
  logoutCloudflare: () => invoke<void>("logout_cloudflare"),
  remoteSubmit: (device: string, command: RemoteCommand) =>
    invoke<CommandJob>("remote_submit", { device, command }),
  remoteRecent: (device: string) =>
    invoke<RemoteReceipt[]>("remote_recent", { device }),
  remoteQuery: (device: string, requestId: string) =>
    invoke<CommandJob>("remote_query", { device, requestId }),
  remoteOutput: (device: string, dbId: string, jobId: string, after: number) =>
    invoke<JobOutput>("remote_output", { device, dbId, jobId, after }),
  remoteCancel: (device: string, dbId: string, jobId: string) =>
    invoke<CommandJob>("remote_cancel", { device, dbId, jobId }),
  windowVisible: () => invoke<boolean>("window_visible"),
  desktopNavigation: () =>
    invoke<DesktopNavigation | null>("desktop_navigation"),
  status: () => invoke<Status>("status"),
  settings: () => invoke<Settings>("settings"),
  traffic: (period: TrafficPeriod, offset = 0) =>
    invoke<TrafficReport>("traffic", { query: { period, offset, limit: 50 } }),
  invite: (allow: boolean, replace?: string) =>
    invoke<Invitation>("invite", { allow, replace: replace ?? null }),
  cancelInvitation: (invitationId: string) =>
    invoke<void>("cancel_invitation", { invitationId }),
  copyInvitationCommand: (address: string, code: string) =>
    invoke<void>("copy_invitation_command", { address, code }),
  revoke: (device: string) => invoke<Revocation>("revoke", { device }),
  copyInvitation: (link: string) => invoke<void>("copy_invitation", { link }),
  copyIdentifier: (value: string) => invoke<void>("copy_identifier", { value }),
  devices: () =>
    invoke<{
      devices: Device[] | null;
      server_error: { code: string; message: string } | null;
    }>("devices"),
  chooseDirectory: () => invoke<string | null>("choose_directory"),
  action: (request: ActionRequest) =>
    invoke<void>(request.command, "args" in request ? request.args : {}),
  activity: (before: string | null, filter: ActivityFilter) =>
    invoke<{
      db_id: string | null;
      entries: Job[];
      next_cursor: string | null;
    }>("activity_history", { before, filter }),
  job: (dbId: string, id: string) => invoke<Job>("activity_job", { dbId, id }),
  attachment: (id: string) =>
    invoke<AttachmentPreview>("activity_attachment", { id }),
  saveAttachment: (id: string) =>
    invoke<string | null>("save_activity_attachment", { id }),
  output: (dbId: string, job: string, after: number | null) =>
    invoke<JobOutput>("job_output", { dbId, job, after }),
};
