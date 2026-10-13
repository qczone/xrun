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
    cli_service_installed: boolean;
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

export interface PrivateInterface {
  name: string;
  address: string;
  netmask: string;
}
export interface Settings {
  execution: ExecutionSettings;
  attachment_retention_days: number;
  traffic_compression: "auto" | "none" | "zstd";
  lan_address: string | null;
  membership_lease_minutes: number;
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
  connection?: "direct" | "relay" | null;
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
    | "interrupted"
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
  params: { options: ScreenshotOptions };
  result: {
    captured_at: string;
    width: number;
    height: number;
    format: ScreenshotFormat;
    mime: string;
    size: number;
    sha256: string;
    attachment_error: string | null;
  } | null;
};
export type ScreenshotFormat = "png" | "jpeg" | "webp";
export interface ScreenshotOptions {
  max_width: number | null;
  format: ScreenshotFormat;
  quality: number | null;
}
export interface ScreenshotDownload {
  path: string;
  job: Job;
  format: ScreenshotFormat;
  mime: string;
  width: number;
  height: number;
  size: number;
  sha256: string;
  captured_at: string;
  preview: string | null;
}
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

export interface TransferReceipt {
  request_id: string;
  target_device_id: string;
  kind: "push" | "pull";
  label: string;
  expires_at_ms: number;
}
export interface FileTransfer {
  path: string | null;
  remote_path: string;
  size: number;
  sha256: string;
  skipped: boolean;
  resumed_from: number;
  job: FileJob;
}
export interface UploadOptions {
  path: string;
  cwd: string | null;
  mkdir: boolean;
  no_overwrite: boolean;
  expect: string | null;
}
export interface Attachment {
  mime: string | null;
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
  | {
      command: "save_traffic_compression";
      args: { compression: Settings["traffic_compression"] };
    }
  | { command: "save_direct_connection"; args: { address: string | null } }
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
  | "export_authority"
  | "restore_authority"
  | "retire_authority"
  | ActionRequest["command"]
  | "invite"
  | "copy_invitation"
  | "copy_identifier"
  | "revoke"
  | "cloudflare_accounts"
  | "logout_cloudflare"
  | "create_cloudflare_network"
  | "remote_submit"
  | "remote_cancel";

export interface CloudflareAccount {
  id: string;
  name: string;
}
export interface AuthorityBackup {
  network_id: string;
  manager_id: string;
  authority_generation: number;
  roster_version: number;
  transfer_to: string | null;
}
export interface RelayDeployment {
  provider: "cloudflare";
  name: string;
  account_id: string;
  link: string;
  region?: string;
}

export type DeploymentStage =
  | "authorization"
  | "authorization_received"
  | "token_exchange"
  | "accounts"
  | "account_selection"
  | "subdomain"
  | "worker_check"
  | "worker_upload"
  | "address_activation"
  | "deployed"
  | "relay_check"
  | "network_creation"
  | "service_start"
  | "service_connection"
  | "authorization_cleanup"
  | "complete";
export interface DeploymentProgress {
  event: "deployment_progress";
  operation_id: string;
  provider: string;
  operation: string;
  sequence: number;
  timestamp_ms: number;
  stage: DeploymentStage;
  status:
    "running" | "waiting" | "completed" | "failed" | "warning" | "cancelled";
  elapsed_ms: number;
  stage_elapsed_ms: number;
  deployment?: RelayDeployment;
  error?: { code: string; message: string };
}

async function withDeploymentProgress<T>(
  run: (progressId: string) => Promise<T>,
  onProgress?: (progress: DeploymentProgress) => void,
): Promise<T> {
  const progressId = crypto.randomUUID();
  const unlisten = await listen<DeploymentProgress>(
    "xrun-deployment-progress",
    ({ payload }) => {
      if (payload.operation_id === progressId) onProgress?.(payload);
    },
  );
  try {
    return await run(progressId);
  } finally {
    unlisten();
  }
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
  chooseAuthorityArchive: (save: boolean) =>
    invoke<string | null>("choose_authority_archive", { save }),
  exportAuthority: (path: string, password: string, device: string | null) =>
    invoke<AuthorityBackup>("export_authority", { path, password, device }),
  restoreAuthority: (path: string, password: string) =>
    invoke<AuthorityBackup>("restore_authority", { path, password }),
  retireAuthority: () => invoke<void>("retire_authority"),
  connectionDefaults: () => invoke<string>("connection_defaults"),
  cloudflareAccounts: async (
    onAuthorization: () => void,
    reauthorize = false,
    remember = false,
    onProgress?: (progress: DeploymentProgress) => void,
    finishTemporary = false,
  ) => {
    const unlisten = await listen<string>(
      "xrun-cloudflare-authorization",
      onAuthorization,
    );
    try {
      return await withDeploymentProgress(
        (progressId) =>
          invoke<CloudflareAccount[] | null>("cloudflare_accounts", {
            reauthorize,
            remember,
            progressId,
            finishTemporary,
          }),
        onProgress,
      );
    } finally {
      unlisten();
    }
  },
  cancelCloudflareAuthorization: () =>
    invoke<void>("cancel_cloudflare_authorization"),
  openCloudflareAuthorization: () =>
    invoke<void>("open_cloudflare_authorization"),
  createCloudflareNetwork: (
    workerName: string,
    account: string,
    deviceName: string,
    region: string,
    resume: boolean,
    onProgress: (progress: DeploymentProgress) => void,
  ) =>
    withDeploymentProgress(
      (progressId) =>
        invoke<RelayDeployment | null>("create_cloudflare_network", {
          workerName,
          account,
          deviceName,
          region,
          resume,
          progressId,
        }),
      onProgress,
    ),
  cloudflareRelays: () => invoke<RelayDeployment[]>("cloudflare_relays"),
  cloudflareRememberedLogin: () =>
    invoke<boolean>("cloudflare_remembered_login"),
  logoutCloudflare: (onProgress?: (progress: DeploymentProgress) => void) =>
    withDeploymentProgress(
      (progressId) => invoke<void>("logout_cloudflare", { progressId }),
      onProgress,
    ),
  remoteSubmit: (device: string, command: RemoteCommand) =>
    invoke<CommandJob>("remote_submit", { device, command }),
  chooseScreenshotDestination: (format: ScreenshotFormat) =>
    invoke<string | null>("choose_screenshot_destination", { format }),
  remoteScreenshot: (
    device: string,
    path: string,
    options: ScreenshotOptions,
  ) =>
    invoke<ScreenshotDownload>("remote_screenshot", { device, path, options }),
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
  chooseTransferFile: (upload: boolean, name: string) =>
    invoke<string | null>("choose_transfer_file", { upload, name }),
  fileReceipts: (device: string) =>
    invoke<TransferReceipt[]>("remote_file_receipts", { device }),
  fileUpload: (device: string, local: string, options: UploadOptions) =>
    invoke<FileTransfer>("remote_file_upload", { device, local, options }),
  fileDownload: (
    device: string,
    remote: string,
    local: string,
    cwd: string | null,
  ) =>
    invoke<FileTransfer>("remote_file_download", {
      device,
      remote,
      local,
      cwd,
    }),
  fileResume: (device: string, request_id: string) =>
    invoke<FileTransfer>("remote_file_resume", { device, request_id }),
  fileCancel: (device: string, request_id: string) =>
    invoke<void>("remote_file_cancel", { device, request_id }),
  privateInterfaces: () => invoke<PrivateInterface[]>("private_interfaces"),
  settings: () => invoke<Settings>("settings"),
  saveMembershipLease: (minutes: number) =>
    invoke<void>("save_membership_lease", { minutes }),
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
