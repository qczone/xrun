import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type { Job, Device, Status, ActivityFilter } from "../api";
import { Icon } from "../components/Icon";
import { ErrorNotice } from "../components/ErrorNotice";
import { RefreshButton } from "../components/RefreshButton";
import { compactIdentifier } from "../format";
import { formatLocale, t } from "../i18n";
import { CommandActivityDetail } from "./activity/CommandActivityDetail";
import { ActivityDetail } from "./activity/ActivityDetail";
import { ActivityRow } from "./activity/ActivityRow";
import { jobDescription, operationLabel } from "./activity/operation";
import { useActivityFeed } from "./activity/useActivityFeed";

interface Props {
  active: boolean;
  paused: boolean;
  status: Status | null;
  devices: Device[];
  feedback: ReactNode;
}
function dayLabel(time: number) {
  const date = new Date(time);
  const today = new Date();
  const yesterday = new Date();
  yesterday.setDate(today.getDate() - 1);
  if (date.toDateString() === today.toDateString()) return t("activity.today");
  if (date.toDateString() === yesterday.toDateString())
    return t("activity.yesterday");
  return date.toLocaleDateString(formatLocale(), {
    year: "numeric",
    month: "long",
    day: "numeric",
  });
}
export function ActivityPage({
  active,
  paused,
  status,
  devices,
  feedback,
}: Props) {
  const [selection, setSelection] = useState<{ job: Job; dbId: string } | null>(
    null,
  );
  const [detailLoading, setDetailLoading] = useState(false);
  const [search, setSearch] = useState("");
  const selectionOrigin = useRef<HTMLButtonElement | null>(null);
  const activityFeed = useActivityFeed(active, paused, selection !== null);
  const {
    filter,
    pagination,
    nextCursor,
    jobs,
    dbId,
    loading,
    refreshing,
    error,
    refreshVersion,
    refreshActivities,
  } = activityFeed;
  const deviceLabel = useCallback(
    (id: string) => {
      if (id === status?.local.device_id)
        return status.local.name || compactIdentifier(id);
      return (
        devices.find((device) => device.device_id === id)?.name ||
        compactIdentifier(id)
      );
    },
    [status, devices],
  );
  const closeDetails = useCallback(() => {
    setSelection(null);
    setDetailLoading(false);
    selectionOrigin.current?.focus({ preventScroll: true });
  }, []);
  useEffect(() => {
    if (!active || !selection) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (
        event.key === "Escape" &&
        !paused &&
        !document.querySelector("dialog[open]")
      )
        closeDetails();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [active, selection, paused, closeDetails]);
  const groups: { day: string; jobs: Job[] }[] = [];
  const query = search.toLocaleLowerCase();
  for (const job of jobs) {
    if (
      !`${operationLabel(job.kind)} ${jobDescription(job)} ${deviceLabel(job.source_device_id)} ${job.source_device_id}`
        .toLocaleLowerCase()
        .includes(query)
    )
      continue;
    const day = dayLabel(job.created_at_ms);
    const group = groups.at(-1);
    if (group?.day === day) group.jobs.push(job);
    else groups.push({ day, jobs: [job] });
  }
  const selectFilter = (value: ActivityFilter) => {
    closeDetails();
    setSearch("");
    activityFeed.selectFilter(value);
  };
  const detailProps = {
    active,
    paused,
    refreshVersion,
    deviceLabel,
    onLoading: setDetailLoading,
    retry: refreshActivities,
    onClose: closeDetails,
  };
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>{t("activity.title")}</h1>
          <p>{t("activity.description")}</p>
        </div>
        <RefreshButton
          refreshing={selection ? detailLoading : refreshing}
          onClick={refreshActivities}
        />
      </div>
      {feedback}
      <div className={`activity-layout ${selection ? "has-selection" : ""}`}>
        <div className="activity-list">
          <div className="list-toolbar">
            <div
              className="filter-tabs"
              role="group"
              aria-label={t("activity.activityState")}
            >
              {(["all", "running", "failed"] as const).map((value) => (
                <button
                  key={value}
                  className={`filter-tab ${filter === value ? "selected" : ""}`}
                  aria-pressed={filter === value}
                  onClick={() => selectFilter(value)}
                >
                  {t(
                    value === "all"
                      ? "activity.allActivities"
                      : value === "running"
                        ? "activity.running"
                        : "activity.failed",
                  )}
                </button>
              ))}
            </div>
            <label className="list-search">
              <Icon name="search" />
              <input
                type="text"
                value={search}
                aria-label={t("activity.search")}
                placeholder={t("activity.search")}
                onChange={(event) => setSearch(event.target.value)}
              />
            </label>
          </div>
          <section
            className="activity-record-panel"
            aria-label={t("activity.timeline")}
            aria-busy={refreshing}
          >
            <div id="activity-records">
              {groups.map((group) => (
                <div className="timeline-day" key={group.day}>
                  <h2>{group.day}</h2>
                  <ol className="activity-timeline">
                    {group.jobs.map((job) => (
                      <li key={job.job_id}>
                        <ActivityRow
                          job={job}
                          sourceName={deviceLabel(job.source_device_id)}
                          active={active && !paused && !selection}
                          selected={selection?.job.job_id === job.job_id}
                          onSelect={(button) => {
                            selectionOrigin.current = button;
                            activityFeed.clearLoading();
                            activityFeed.clearError();
                            setSelection({ job, dbId: dbId || job.db_id });
                          }}
                        />
                      </li>
                    ))}
                  </ol>
                </div>
              ))}
            </div>
            {!groups.length && !error && (
              <p className="empty-state">
                {loading
                  ? t("activity.loading")
                  : search || filter !== "all"
                    ? t("activity.emptyFiltered")
                    : t("activity.empty")}
              </p>
            )}
          </section>
          <div className="activity-pagination">
            <button
              className="text-button"
              disabled={loading || pagination.index === 0 || selection !== null}
              onClick={activityFeed.previousPage}
            >
              {t("activity.previous")}
            </button>
            <span className="muted">
              {t("activity.page", { page: pagination.index + 1 })}
            </span>
            <button
              className="text-button"
              disabled={loading || nextCursor === null || selection !== null}
              onClick={activityFeed.nextPage}
            >
              {t("activity.next")}
            </button>
          </div>
          {error && (
            <ErrorNotice
              title={t("activity.loadFailed")}
              detail={error}
              retry={refreshActivities}
              dismiss={activityFeed.clearError}
            />
          )}
        </div>
        {selection && (
          <aside className="activity-detail" aria-label={t("activity.details")}>
            {selection.job.kind === "exec" ? (
              <CommandActivityDetail
                key={`${selection.dbId}:${selection.job.job_id}`}
                selection={{ job: selection.job, dbId: selection.dbId }}
                status={status}
                {...detailProps}
              />
            ) : (
              <ActivityDetail
                key={`${selection.dbId}:${selection.job.job_id}`}
                selection={selection}
                {...detailProps}
              />
            )}
          </aside>
        )}
      </div>
    </>
  );
}
