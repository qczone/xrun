import type { Job } from "../../api";
import { Icon } from "../../components/Icon";
import { duration, fileSize } from "../../format";
import { formatLocale } from "../../i18n";
import { ActivityState } from "./ActivityState";
import { ScreenshotThumbnail } from "./ScreenshotThumbnail";
import {
  operationLabel,
  jobDescription,
  jobDuration,
  jobSize,
  attachmentLabel,
} from "./operation";

interface Props {
  job: Job;
  sourceName: string;
  active: boolean;
  selected: boolean;
  onSelect: (button: HTMLButtonElement) => void;
}
export function ActivityRow({
  job,
  sourceName,
  active,
  selected,
  onSelect,
}: Props) {
  const elapsed = jobDuration(job);
  const attachment = job.attachments[0];
  const hasAttachment = ["push", "pull", "screenshot"].includes(job.kind);
  const description = jobDescription(job);
  const kindIcon =
    job.kind === "exec" || job.kind === "stream_exec"
      ? "terminal"
      : job.kind === "screenshot"
        ? "image"
        : job.kind === "push"
          ? "down"
          : job.kind === "pull"
            ? "up"
            : "arrow";
  return (
    <button
      className={`activity-row ${selected ? "selected" : ""}`}
      data-job-id={job.job_id}
      aria-pressed={selected}
      onClick={(event) => onSelect(event.currentTarget)}
    >
      <span className="timeline-node">
        <Icon name={kindIcon} />
      </span>
      <span className="activity-row-content">
        <span className="activity-row-header">
          <strong>{operationLabel(job.kind)}</strong>
          <ActivityState job={job} />
          <time dateTime={new Date(job.created_at_ms).toISOString()}>
            {new Date(job.created_at_ms).toLocaleTimeString(formatLocale(), {
              hour: "2-digit",
              minute: "2-digit",
              hour12: false,
            })}
          </time>
        </span>
        <span className="activity-row-meta">
          {sourceName}
          {fileSize(jobSize(job))}
          {elapsed !== null && ` · ${duration(elapsed)}`}
        </span>
        {description && (!hasAttachment || !attachment) && (
          <span className="activity-row-description mono" title={description}>
            {description}
          </span>
        )}
        {hasAttachment && (
          <span
            className={`attachment-tag ${attachment?.status === "available" ? "available" : ""}`}
          >
            <Icon name={job.kind === "screenshot" ? "image" : "folder"} />
            {attachmentLabel(attachment ?? null)}
          </span>
        )}
      </span>
      {job.kind === "screenshot" && attachment && (
        <ScreenshotThumbnail attachment={attachment} active={active} />
      )}
    </button>
  );
}
