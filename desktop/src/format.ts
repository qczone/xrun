import type { Job, Status } from "./api";
import { formatLocale, t } from "./i18n";

export function compactIdentifier(value: string) {
  const identifier = value.replace(/^net_/, "");
  return identifier.length > 12
    ? `${identifier.slice(0, 8)}…${identifier.slice(-4)}`
    : identifier;
}

export const osName = (os: string | null | undefined) =>
  ({ macos: "macOS", windows: "Windows", linux: "Linux" })[os || ""] ||
  os ||
  t("format.unknownOs");
export const archName = (arch: string | null | undefined) =>
  ({ aarch64: "ARM64", arm64: "ARM64", x86_64: "x64", x86: "x86" })[
    arch || ""
  ] ||
  arch ||
  "";
export const dateTime = (time: number) =>
  new Date(time).toLocaleString(formatLocale(), { hour12: false });
export const clockTime = (time: number) =>
  new Date(time).toLocaleTimeString(formatLocale());
export const isRunning = (job: Job) =>
  job.state === "accepted" || job.state === "running";

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

export function serviceState(status: Status | null): [string, string] {
  if (!status) return [t("common.checking"), "neutral"];
  if (status.service.approval_required)
    return [t("status.systemApproval"), "warning"];
  return status.local.daemon_running
    ? [t("status.running"), "online"]
    : [t("status.stopped"), "neutral"];
}

/** One summary of this device: blocking states first, then relay connectivity. */
export function localState(status: Status): [string, string] {
  const { local, service } = status;
  if (!local.joined) return [t("status.noNetwork"), "neutral"];
  if (service.approval_required) return [t("status.systemApproval"), "warning"];
  if (!local.daemon_running) return [t("status.noService"), "neutral"];
  if (local.remote_access_paused) return [t("status.paused"), "warning"];
  if (local.daemon_connected === null) return [t("status.starting"), "neutral"];
  return local.daemon_connected
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

export function jobState(job: Job): [string, string] {
  if (
    job.state === "failed" &&
    (job.kind === "exec" || job.kind === "stream_exec") &&
    job.result &&
    (job.result.exit_code !== null || job.result.signal !== null)
  )
    return [t("job.nonzeroExit"), "failed"];
  const states = {
    accepted: [t("job.accepted"), "active"],
    running: [t("status.running"), "active"],
    interrupted: [t("job.interrupted"), "neutral"],
    succeeded: [t("job.success"), "success"],
    failed: [t("job.failed"), "failed"],
    canceled: [t("job.canceled"), "neutral"],
    timed_out: [t("job.timedOut"), "failed"],
    lost: [t("job.lost"), "failed"],
  } satisfies Record<Job["state"], [string, string]>;
  return states[job.state];
}
export function commandText(params: { program: string; args: string[] }) {
  return [params.program || t("job.script"), ...params.args]
    .map((arg) => (/[\s"'\\]/.test(arg) ? JSON.stringify(arg) : arg))
    .join(" ");
}

export function fileSize(size: number | null) {
  if (size === null) return "";
  if (size < 1024) return ` · ${size} B`;
  if (size < 1024 * 1024) return ` · ${(size / 1024).toFixed(1)} KiB`;
  return ` · ${(size / (1024 * 1024)).toFixed(1)} MiB`;
}
