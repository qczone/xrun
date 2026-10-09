import { useCallback, useState } from "react";
import type { Job, Device, Status, TaskFilter } from "../api";
import { Icon } from "../components/Icon";
import { ErrorNotice } from "../components/ErrorNotice";
import { duration, fileSize } from "../format";
import { formatLocale, t } from "../i18n";
import { StateChip } from "./history/TaskState";
import { TaskDetail } from "./history/TaskDetail";
import { JobDetail } from "./history/JobDetail";
import {
  operationLabel,
  jobDescription,
  jobSize,
  jobDuration,
  attachmentLabel,
} from "./history/operation";
import { useRecordHistory } from "./history/useRecordHistory";
interface Props {
  active: boolean;
  paused: boolean;
  status: Status | null;
  devices: Device[];
}
function dayLabel(time: number) {
  const date = new Date(time);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (date.toDateString() === today.toDateString()) return t("history.today");
  if (date.toDateString() === yesterday.toDateString())
    return t("history.yesterday");
  return date.toLocaleDateString(formatLocale(), {
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}
export function History({ active, paused, status, devices }: Props) {
  const [selected, setSelected] = useState<{
    job: Job;
    dbId: string;
  } | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const history = useRecordHistory(active, paused, selected !== null);
  const {
    filter,
    pagination,
    next,
    entries,
    dbId,
    loading,
    error,
    refresh,
    refreshRecords,
  } = history;
  const deviceLabel = useCallback(
    (id: string) => {
      if (id === status?.local.device_id) return status.local.name || id;
      return devices.find((device) => device.device_id === id)?.name || id;
    },
    [status, devices],
  );
  const groups: { day: string; entries: Job[] }[] = [];
  for (const entry of entries) {
    const day = dayLabel(entry.created_at_ms);
    const group = groups.at(-1);
    if (group?.day === day) group.entries.push(entry);
    else groups.push({ day, entries: [entry] });
  }
  const back = () => {
    setSelected(null);
    setDetailLoading(false);
    history.clearError();
  };
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>{t("history.title")}</h1>
          <p>{t("history.description")}</p>
        </div>
        <button
          disabled={selected ? detailLoading : loading}
          onClick={refreshRecords}
        >
          <Icon name="refresh" />
          {(selected ? detailLoading : loading)
            ? t("common.refreshing")
            : t("common.refresh")}
        </button>
      </div>
      <div className={`history-layout ${selected ? "has-selection" : ""}`}>
        <div className="history-list">
          <div className="history-toolbar">
            <span className="journey-order">{t("history.newestFirst")}</span>
            <label>
              {t("history.state")}
              <select
                aria-label={t("history.activityState")}
                value={filter}
                onChange={(event) => {
                  history.selectFilter(event.target.value as TaskFilter);
                  back();
                }}
              >
                <option value="all">{t("history.allActivities")}</option>
                <option value="running">{t("history.running")}</option>
                <option value="failed">{t("history.failed")}</option>
              </select>
            </label>
          </div>
          <section
            className="panel history-record-panel"
            aria-label={t("history.timeline")}
          >
            <div id="history-records">
              {groups.map((group) => (
                <div className="journey-day" key={group.day}>
                  <h2>{group.day}</h2>
                  <ol className="journey-timeline">
                    {group.entries.map((entry) => {
                      const job = entry;
                      return (
                        <li key={entry.job_id}>
                          <button
                            className={`task-record journey-record ${selected?.job.job_id === entry.job_id ? "selected" : ""}`}
                            aria-pressed={selected?.job.job_id === entry.job_id}
                            onClick={() => {
                              history.clearLoading();
                              history.clearError();
                              setSelected({
                                job: entry,
                                dbId: dbId || job?.db_id || "",
                              });
                            }}
                          >
                            <span className="journey-node">
                              <Icon
                                name={
                                  job.kind === "exec" ||
                                  job.kind === "stream_exec"
                                    ? "terminal"
                                    : job.kind === "screenshot"
                                      ? "monitor"
                                      : job.kind === "forward"
                                        ? "arrow"
                                        : "folder"
                                }
                              />
                            </span>
                            <span className="record-header">
                              <strong>{operationLabel(job.kind)}</strong>
                              <StateChip job={job} />
                            </span>
                            {jobDescription(job) && (
                              <span className="record-command mono">
                                {jobDescription(job)}
                              </span>
                            )}
                            <span className="record-meta">
                              {t("history.source", {
                                name: deviceLabel(job.source_device_id),
                              })}
                              {fileSize(jobSize(job))}
                              {jobDuration(job) !== null &&
                                ` · ${duration(jobDuration(job)!)}`}
                            </span>
                            {["push", "pull", "screenshot"].includes(
                              job.kind,
                            ) && (
                              <span
                                className={`attachment-tag ${job.attachments[0]?.status === "available" ? "available" : ""}`}
                              >
                                <Icon name="folder" />{" "}
                                {attachmentLabel(job.attachments[0] ?? null)}
                              </span>
                            )}
                            <span className="record-secondary">
                              {job && <code>{job.job_id}</code>}
                              <time
                                dateTime={new Date(
                                  entry.created_at_ms,
                                ).toISOString()}
                              >
                                {new Date(
                                  entry.created_at_ms,
                                ).toLocaleTimeString(formatLocale(), {
                                  hour: "2-digit",
                                  minute: "2-digit",
                                  hour12: false,
                                })}
                              </time>
                            </span>
                          </button>
                        </li>
                      );
                    })}
                  </ol>
                </div>
              ))}
            </div>
            {!entries.length && !error && (
              <p className="empty-state">
                {loading
                  ? t("history.loading")
                  : filter === "all"
                    ? t("history.empty")
                    : t("history.emptyFiltered")}
              </p>
            )}
          </section>
          <div className="history-pagination">
            <button
              disabled={loading || pagination.index === 0 || selected !== null}
              onClick={history.previousPage}
            >
              {t("history.previous")}
            </button>
            <span className="muted">
              {t("history.page", { page: pagination.index + 1 })}
            </span>
            <button
              disabled={loading || next === null || selected !== null}
              onClick={history.nextPage}
            >
              {t("history.next")}
            </button>
          </div>
          {error && (
            <ErrorNotice
              title={t("history.loadFailed")}
              detail={error}
              retry={refreshRecords}
              dismiss={history.clearError}
            />
          )}
        </div>
        {selected?.job.kind === "exec" && (
          <TaskDetail
            key={`${selected.dbId}:${selected.job.job_id}`}
            selection={{ job: selected.job, dbId: selected.dbId }}
            active={active}
            paused={paused}
            refresh={refresh}
            status={status}
            deviceLabel={deviceLabel}
            onLoading={setDetailLoading}
            retry={refreshRecords}
            back={back}
          />
        )}
        {selected && selected.job.kind !== "exec" && (
          <JobDetail
            key={`${selected.dbId}:${selected.job.job_id}`}
            selection={selected}
            active={active}
            paused={paused}
            refresh={refresh}
            deviceLabel={deviceLabel}
            onLoading={setDetailLoading}
            retry={refreshRecords}
            back={back}
          />
        )}
      </div>
      <p className="footnote">{t("history.footnote")}</p>
    </>
  );
}
