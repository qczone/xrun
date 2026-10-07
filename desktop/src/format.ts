import type { Job, Status } from "./api";
import { formatLocale, t } from "./i18n";

export const osName = (os: string | null | undefined) =>
  ({ macos: "macOS", windows: "Windows", linux: "Linux" })[os || ""] ||
  os ||
  t("format.unknownOs");
export const recordTime = (time: number) =>
  new Date(time).toLocaleString(formatLocale(), { hour12: false });
export const clockTime = (time: number) =>
  new Date(time).toLocaleTimeString(formatLocale());
export const isRunning = (job: Job) =>
  job.state === "starting" || job.state === "running";

export function duration(ms: number | null) {
  if (ms === null) return "—";
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60000)
    return t("format.seconds", { seconds: (ms / 1000).toFixed(1) });
  return t("format.minutes", {
    minutes: Math.floor(ms / 60000),
    seconds: Math.floor((ms % 60000) / 1000),
  });
}

export function serviceLabel(status: Status | null): [string, string] {
  if (!status) return [t("status.check"), t("status.reading")];
  const { local, service } = status;
  if (!local.joined) return [t("status.notJoined"), t("status.joinHint")];
  if (service.approval_required)
    return [t("status.approval"), t("status.approvalHint")];
  if (!local.daemon_running)
    return [t("status.stopped"), t("status.stoppedHint")];
  if (local.remote_access_paused)
    return [t("status.paused"), t("status.pausedHint")];
  if (local.daemon_connected === true)
    return [t("status.connected"), t("status.connectedHint")];
  if (local.daemon_connected === null)
    return [t("status.running"), t("status.olderServiceHint")];
  return [t("status.connecting"), t("status.connectingHint")];
}

export function serviceState(status: Status | null): [string, string] {
  if (!status) return [t("common.checking"), "neutral"];
  if (status.service.approval_required)
    return [t("status.systemApproval"), "warning"];
  return status.local.daemon_running
    ? [t("status.running"), "online"]
    : [t("status.stopped"), "neutral"];
}

export function relayState(status: Status | null): [string, string] {
  if (!status) return [t("common.checking"), "neutral"];
  if (!status.local.joined) return [t("status.noNetwork"), "neutral"];
  if (!status.local.daemon_running) return [t("status.noService"), "neutral"];
  if (status.local.daemon_connected === null)
    return [t("status.unavailable"), "neutral"];
  return status.local.daemon_connected
    ? [t("status.connected"), "online"]
    : [t("status.reconnecting"), "warning"];
}

export function relayHost(address: string) {
  try {
    return new URL(address).host;
  } catch {
    return t("status.relayDetails");
  }
}

export { errorText } from "./errors";

export function taskState(job: Job): [string, string] {
  if (job.state === "exited")
    return job.exit_code === 0 && !job.signal
      ? [t("task.success"), "success"]
      : [t("task.nonzeroExit"), "failed"];
  const states = {
    starting: [t("task.starting"), "active"],
    running: [t("status.running"), "active"],
    failed: [t("task.failed"), "failed"],
    canceled: [t("task.canceled"), "neutral"],
    timed_out: [t("task.timedOut"), "failed"],
    lost: [t("task.lost"), "failed"],
  } satisfies Record<string, [string, string]>;
  return states[job.state];
}

export function commandText(job: Job) {
  return [job.program || t("task.script"), ...job.args]
    .map((arg) => (/[\s"'\\]/.test(arg) ? JSON.stringify(arg) : arg))
    .join(" ");
}

export function fileSize(size: number | null) {
  if (size === null) return "";
  if (size < 1024) return ` · ${size} B`;
  if (size < 1024 * 1024) return ` · ${(size / 1024).toFixed(1)} KiB`;
  return ` · ${(size / (1024 * 1024)).toFixed(1)} MiB`;
}
