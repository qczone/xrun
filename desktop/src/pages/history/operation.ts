import type { Attachment, Job } from "../../api";
import { commandText } from "../../format";
import { t } from "../../i18n";

export function operationLabel(op: string) {
  const labels: Record<string, string> = {
    exec: t("history.command"),
    push: t("history.receiveFile"),
    pull: t("history.sendFile"),
    screenshot: t("history.screenshot"),
    stream_exec: t("history.streamCommand"),
    forward: t("history.forward"),
  };
  return labels[op] || op;
}

export function jobDescription(job: Job) {
  if (job.kind === "exec" || job.kind === "stream_exec")
    return commandText(job.params);
  if (job.kind === "push" || job.kind === "pull")
    return job.result?.path || job.params.path;
  if (job.kind === "forward") return `localhost:${job.params.port}`;
  return "";
}
export function jobSize(job: Job) {
  return job.result && "size" in job.result
    ? job.result.size
    : job.kind === "push"
      ? job.params.size
      : null;
}
export function jobDuration(job: Job) {
  return job.result && "duration_ms" in job.result
    ? job.result.duration_ms
    : null;
}
export function attachmentLabel(attachment: Attachment | null) {
  if (!attachment) return t("attachment.notRetained");
  if (attachment.status === "expired") return t("attachment.expired");
  if (attachment.status === "missing") return t("attachment.missing");
  return attachment.name;
}
