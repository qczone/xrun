import { useCallback, useState } from "react";
import type { Device, Status, TaskFilter } from "../api";
import { Icon } from "../components/Icon";
import { ErrorNotice } from "../components/ErrorNotice";
import { commandText, duration, fileSize, recordTime } from "../format";
import { t } from "../i18n";
import { StateChip } from "./history/TaskState";
import { TaskDetail } from "./history/TaskDetail";
import type { Selection } from "./history/useTaskOutput";
import { useRecordHistory } from "./history/useRecordHistory";
interface Props {
  active: boolean;
  paused: boolean;
  status: Status | null;
  devices: Device[];
}
export function History({ active, paused, status, devices }: Props) {
  const [selected, setSelected] = useState<Selection | null>(null);
  const [outputLoading, setOutputLoading] = useState(false);
  const history = useRecordHistory(active, paused, selected !== null);
  const {
    tab,
    filter,
    pagination,
    next,
    jobs,
    files,
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
  const records = tab === "tasks" ? jobs : files;

  return (
    <>
      <div className="page-heading">
        <div>
          <h1>{t("history.title")}</h1>
          <p>{t("history.description")}</p>
        </div>
        <button
          disabled={selected ? outputLoading : loading}
          onClick={refreshRecords}
        >
          <Icon name="refresh" />
          {(selected ? outputLoading : loading)
            ? t("common.refreshing")
            : t("common.refresh")}
        </button>
      </div>
      <div className={`history-layout ${selected ? "has-selection" : ""}`}>
        <div className="history-list">
          <div className="history-toolbar">
            <div
              className="segmented"
              role="group"
              aria-label={t("history.recordType")}
            >
              <button
                aria-pressed={tab === "tasks"}
                className={tab === "tasks" ? "selected" : ""}
                onClick={() => {
                  history.selectTab("tasks");
                  setSelected(null);
                }}
              >
                {t("history.tasks")}
              </button>
              <button
                aria-pressed={tab === "files"}
                className={tab === "files" ? "selected" : ""}
                onClick={() => {
                  history.selectTab("files");
                  setSelected(null);
                }}
              >
                {t("history.files")}
              </button>
            </div>
            {tab === "tasks" && (
              <label>
                {t("history.state")}
                <select
                  aria-label={t("history.taskState")}
                  value={filter}
                  onChange={(event) => {
                    history.selectFilter(event.target.value as TaskFilter);
                    setSelected(null);
                  }}
                >
                  <option value="all">{t("history.allTasks")}</option>
                  <option value="running">{t("history.running")}</option>
                  <option value="failed">{t("history.failed")}</option>
                </select>
              </label>
            )}
          </div>
          <section className="panel history-record-panel">
            <div id="history-records">
              {tab === "tasks"
                ? jobs.map((job) => (
                    <button
                      className={`task-record ${selected?.job.job_id === job.job_id ? "selected" : ""}`}
                      key={job.job_id}
                      aria-pressed={selected?.job.job_id === job.job_id}
                      onClick={() => {
                        history.clearLoading();
                        history.clearError();
                        setSelected({ job, dbId: dbId || job.db_id });
                      }}
                    >
                      <span className="record-header">
                        <strong className="record-command mono">
                          {commandText(job)}
                        </strong>
                        <StateChip job={job} />
                      </span>
                      <span className="record-meta">
                        {t("history.source", {
                          name: deviceLabel(job.source_device_id),
                        })}{" "}
                        ·{" "}
                        {job.duration_ms === null
                          ? t("history.viewOutput")
                          : t("history.duration", {
                              duration: duration(job.duration_ms),
                            })}
                      </span>
                      <span className="record-secondary">
                        <code>{job.job_id}</code>
                        <span>{recordTime(job.created_at_ms)}</span>
                      </span>
                    </button>
                  ))
                : files.map((record, index) => (
                    <article
                      className="file-record"
                      key={`${record.time_ms}:${index}`}
                    >
                      <div className="record-header">
                        <strong>
                          {(
                            {
                              push: t("history.receiveFile"),
                              pull: t("history.sendFile"),
                              screenshot: t("history.screenshot"),
                            } as Record<string, string>
                          )[record.op] || record.op}
                        </strong>
                        <span
                          className={`task-state ${record.result === "ok" ? "success" : "failed"}`}
                        >
                          {record.result === "ok"
                            ? t("history.completed")
                            : t("history.interrupted")}
                        </span>
                      </div>
                      {record.path && (
                        <p className="record-command mono">{record.path}</p>
                      )}
                      <p className="record-meta">
                        {t("history.source", {
                          name: deviceLabel(record.source_device_id),
                        })}
                        {fileSize(record.size)}
                      </p>
                      <span className="record-secondary">
                        {recordTime(record.time_ms)}
                      </span>
                    </article>
                  ))}
            </div>
            {!records.length && !error && (
              <p className="empty-state">
                {loading
                  ? t("history.loading")
                  : tab === "files"
                    ? t("history.emptyFiles")
                    : filter === "all"
                      ? t("history.emptyTasks")
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
        {selected && (
          <TaskDetail
            key={`${selected.dbId}:${selected.job.job_id}`}
            selection={selected}
            active={active}
            paused={paused}
            refresh={refresh}
            status={status}
            deviceLabel={deviceLabel}
            onLoading={setOutputLoading}
            retry={refreshRecords}
            back={() => {
              setSelected(null);
              history.clearError();
            }}
          />
        )}
      </div>
      <p className="footnote">{t("history.footnote")}</p>
    </>
  );
}
