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
} from "./operation";

interface Props {
  job: Job;
  sourceName: string;
  active: boolean;
  selected: boolean;
  onSelect: (button: HTMLButtonElement) => void;
}
/** The subject leads each row; the operation, source and size follow. */
function rowTitle(job: Job) {
  const description = jobDescription(job);
  if (job.kind === "push" || job.kind === "pull")
    return description.split(/[\\/]/).filter(Boolean).pop() || description;
  if (job.kind === "screenshot")
    return (
      job.attachments[0]?.name ||
      (job.result
        ? `${job.result.width} × ${job.result.height}`
        : operationLabel(job.kind))
    );
  return description || operationLabel(job.kind);
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
  const description = jobDescription(job);
  const command = job.kind === "exec" || job.kind === "stream_exec";
  const kindIcon = command
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
      <span className="activity-icon">
        <Icon name={kindIcon} />
      </span>
      <span className="activity-row-content">
        <span
          className={`activity-row-title ${command || job.kind === "forward" ? "mono" : ""}`}
          title={description || undefined}
        >
          {rowTitle(job)}
        </span>
        <span className="activity-row-meta">
          <span className="activity-kind">{operationLabel(job.kind)}</span>
          <span>
            {sourceName}
            {fileSize(jobSize(job))}
            {elapsed !== null && ` · ${duration(elapsed)}`}
          </span>
        </span>
      </span>
      {job.kind === "screenshot" && attachment && (
        <ScreenshotThumbnail attachment={attachment} active={active} />
      )}
      <span className="activity-row-side">
        <ActivityState job={job} />
        <time dateTime={new Date(job.created_at_ms).toISOString()}>
          {new Date(job.created_at_ms).toLocaleTimeString(formatLocale(), {
            hour: "2-digit",
            minute: "2-digit",
            hour12: false,
          })}
        </time>
      </span>
    </button>
  );
}
