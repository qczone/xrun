import { useEffect, useState } from "react";
import { api, type Job } from "../../api";
import { errorCode } from "../../errors";
import { Icon } from "../../components/Icon";
import { ErrorNotice } from "../../components/ErrorNotice";
import { duration, errorText, isRunning, dateTime } from "../../format";
import { t } from "../../i18n";
import { ActivityState } from "./ActivityState";
import { AttachmentPanel } from "./AttachmentPanel";
import { jobDescription, jobDuration, operationLabel } from "./operation";
interface Props {
  selection: { job: Job; dbId: string };
  active: boolean;
  paused: boolean;
  refreshVersion: number;
  deviceLabel: (id: string) => string;
  onLoading: (value: boolean) => void;
  retry: () => void;
  onClose: () => void;
}
export function ActivityDetail({
  selection,
  active,
  paused,
  refreshVersion,
  deviceLabel,
  onLoading,
  retry,
  onClose,
}: Props) {
  const [job, setJob] = useState(selection.job);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [previewLoading, setPreviewLoading] = useState(false);
  const {
    dbId,
    job: { job_id: id },
  } = selection;
  useEffect(() => {
    if (!active || paused) return;
    let disposed = false;
    let pending = false;
    let finished = false;
    const load = async (force = false) => {
      if (disposed || pending || (finished && !force)) return;
      pending = true;
      setLoading(true);
      try {
        const value = await api.job(dbId, id);
        if (disposed) return;
        setJob(value);
        setError(null);
        finished = !isRunning(value);
      } catch (error) {
        if (!disposed) {
          setError(errorText(error));
          if (
            [
              "DB_RESET",
              "JOB_NOT_FOUND",
              "DB_MISSING",
              "DB_SCHEMA_MISMATCH",
              "DB_CORRUPT",
            ].includes(errorCode(error) ?? "")
          )
            finished = true;
        }
      } finally {
        pending = false;
        if (!disposed) setLoading(false);
      }
    };
    queueMicrotask(() => void load(true));
    const timer = setInterval(() => void load(), 3000);
    return () => {
      disposed = true;
      clearInterval(timer);
    };
  }, [active, paused, refreshVersion, dbId, id]);
  useEffect(() => {
    onLoading(loading || previewLoading);
    return () => onLoading(false);
  }, [loading, previewLoading, onLoading]);
  const elapsed = jobDuration(job);
  const command =
    job.kind === "stream_exec" || job.kind === "exec" ? job.result : null;
  const cwd = "cwd" in job.params ? job.params.cwd : null;
  const attachmentError =
    job.result && "attachment_error" in job.result
      ? job.result.attachment_error
      : null;
  const hasAttachment = ["push", "pull", "screenshot"].includes(job.kind);
  return (
    <section id="activity-job-detail">
      <button
        className="activity-close icon-button"
        aria-label={t("activity.closeDetails")}
        onClick={onClose}
      >
        <Icon name="close" />
      </button>
      <article className="panel padded">
        <div className="activity-detail-title">
          <h2>{operationLabel(job.kind)}</h2>
          <ActivityState job={job} />
        </div>
        {jobDescription(job) && (
          <p className="activity-description mono">{jobDescription(job)}</p>
        )}
        <div className="activity-detail-meta">
          <span>
            {t("activity.source", { name: deviceLabel(job.source_device_id) })}
          </span>
          <span>{dateTime(job.created_at_ms)}</span>
        </div>
        <details className="activity-detail-facts">
          <summary>{t("activity.technicalDetails")}</summary>
          <div className="panel-row">
            <span>Job ID</span>
            <code>{job.job_id}</code>
          </div>
          <div className="panel-row">
            <span>{t("output.sourceId")}</span>
            <code>{job.source_device_id}</code>
          </div>
          <div className="panel-row">
            <span>request_id</span>
            <code>{job.request_id}</code>
          </div>
          {cwd && (
            <div className="panel-row">
              <span>{t("output.cwd")}</span>
              <code>{cwd}</code>
            </div>
          )}
          {job.started_at_ms !== null && (
            <div className="panel-row">
              <span>{t("output.startedAt")}</span>
              <span>{dateTime(job.started_at_ms)}</span>
            </div>
          )}
        </details>
        {elapsed !== null && (
          <div className="panel-row">
            <span>
              {t("activity.duration", { duration: duration(elapsed) })}
            </span>
            {command && (
              <span>
                {command.signal !== null
                  ? t("output.signal", { signal: command.signal })
                  : command.exit_code !== null
                    ? t("output.exitCode", { code: command.exit_code })
                    : t("output.noExitCode")}
              </span>
            )}
          </div>
        )}
        {job.finished_at_ms !== null && (
          <div className="panel-row">
            <span>{t("activity.endedAt")}</span>
            <span>{dateTime(job.finished_at_ms)}</span>
          </div>
        )}
        {job.error_message && (
          <p className="inline-notice">{job.error_message}</p>
        )}
        {job.leftover_possible && (
          <p className="inline-notice">{t("output.leftover")}</p>
        )}
      </article>
      {job.attachments.map((attachment) => (
        <AttachmentPanel
          key={attachment.id}
          metadata={attachment}
          active={active}
          paused={paused}
          refreshVersion={refreshVersion}
          onLoading={setPreviewLoading}
          retry={retry}
        />
      ))}
      {hasAttachment && !job.attachments.length && (
        <article className="panel attachment-panel">
          <h2>{t("attachment.title")}</h2>
          <p className="empty-state">
            {attachmentError
              ? t("attachment.retainFailed")
              : isRunning(job)
                ? t("attachment.loading")
                : t("attachment.notRetainedHint")}
          </p>
          {attachmentError && (
            <p className="inline-notice">{attachmentError}</p>
          )}
        </article>
      )}
      {error && (
        <ErrorNotice
          title={t("activity.loadFailed")}
          detail={error}
          retry={retry}
          dismiss={() => setError(null)}
        />
      )}
    </section>
  );
}
