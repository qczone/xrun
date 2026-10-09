import { useLayoutEffect, useRef, useState } from "react";
import type { Status } from "../../api";
import { ErrorNotice } from "../../components/ErrorNotice";
import { commandText, duration, isRunning, recordTime } from "../../format";
import { t } from "../../i18n";
import { StateChip } from "./TaskState";
import { useJobOutput, type Selection } from "./useJobOutput";
interface DetailProps {
  selection: Selection;
  active: boolean;
  paused: boolean;
  refresh: number;
  status: Status | null;
  deviceLabel: (id: string) => string;
  onLoading: (value: boolean) => void;
  retry: () => void;
  back: () => void;
}

export function TaskDetail({
  selection,
  active,
  paused,
  refresh,
  status,
  deviceLabel,
  onLoading,
  retry,
  back,
}: DetailProps) {
  const { job, error, outputStatus, revision, buffer, clearError } =
    useJobOutput({ selection, active, paused, refresh, onLoading });
  const [stream, setStream] = useState("all");
  const [follow, setFollow] = useState(true);
  const log = useRef<HTMLPreElement>(null);
  const lastScrollTop = useRef(0);
  useLayoutEffect(() => {
    if (follow && log.current) {
      log.current.scrollTop = log.current.scrollHeight;
      lastScrollTop.current = log.current.scrollTop;
    }
  }, [revision, follow, stream]);

  const source = deviceLabel(job.source_device_id);
  const warnings = [];
  if (job.error_message) warnings.push(job.error_message);
  if (!job.output_complete) {
    const reasons: Record<string, string> = {
      TRUNCATED: t("output.truncated"),
      LOG_EXPIRED: t("output.expired"),
      DETACHED_OUTPUT: t("output.detached"),
    };
    warnings.push(
      reasons[job.output_loss_reason || ""] ||
        t("output.incomplete", {
          reason: job.output_loss_reason || t("output.unknownReason"),
        }),
    );
  }
  if (job.leftover_possible) warnings.push(t("output.leftover"));
  if (isRunning(job) && status && !status.local.daemon_running)
    warnings.push(t("output.stale"));
  const elapsed =
    job.result != null
      ? duration(job.result?.duration_ms)
      : isRunning(job)
        ? t("output.elapsed", {
            duration: duration(Math.max(0, Date.now() - job.created_at_ms)),
          })
        : "—";
  const result = job.result?.signal
    ? t("output.signal", { signal: job.result?.signal })
    : job.result?.exit_code != null
      ? t("output.exitCode", { code: job.result?.exit_code })
      : isRunning(job)
        ? t("output.notFinished")
        : t("output.noExitCode");

  return (
    <section id="task-detail">
      <button className="history-back" onClick={back}>
        {t("output.back")}
      </button>
      <article className="panel task-heading">
        <div className="task-title">
          <h2 className="mono">{commandText(job.params)}</h2>
          <StateChip job={job} />
        </div>
        <div className="task-summary-meta">
          <span>{t("history.source", { name: source })}</span>
          <span>{t("history.duration", { duration: elapsed })}</span>
          <span>{result}</span>
        </div>
        <details className="task-facts">
          <summary>{t("output.details", { id: job.job_id })}</summary>
          <div className="panel-row">
            <span>{t("output.sourceId")}</span>
            <code>{job.source_device_id}</code>
          </div>
          <div className="panel-row">
            <span>{t("output.cwd")}</span>
            <code>{job.params.cwd}</code>
          </div>
          <div className="panel-row">
            <span>{t("output.startedAt")}</span>
            <span>{recordTime(job.created_at_ms)}</span>
          </div>
        </details>
      </article>
      {!!warnings.length && (
        <p id="task-warning" className="inline-notice" role="status">
          {warnings.join(" ")}
        </p>
      )}
      <section className="panel output-panel">
        <div className="output-toolbar">
          <strong>{t("output.title")}</strong>
          <label className="sr-only" htmlFor="output-stream">
            {t("output.type")}
          </label>
          <select
            id="output-stream"
            value={stream}
            onChange={(event) => setStream(event.target.value)}
          >
            <option value="all">{t("output.all")}</option>
            <option value="stdout">{t("output.stdout")}</option>
            <option value="stderr">{t("output.stderr")}</option>
          </select>
          <label className="follow-output">
            <input
              type="checkbox"
              checked={follow}
              onChange={(event) => setFollow(event.target.checked)}
            />
            {t("output.follow")}
          </label>
        </div>
        <pre
          id="task-log"
          ref={log}
          className="task-log"
          data-filter={stream}
          tabIndex={0}
          aria-label={t("output.log")}
          onScroll={(event) => {
            const node = event.currentTarget;
            const movingUp = node.scrollTop < lastScrollTop.current;
            lastScrollTop.current = node.scrollTop;
            if (
              follow &&
              movingUp &&
              node.scrollHeight - node.clientHeight - node.scrollTop > 24
            )
              setFollow(false);
          }}
        >
          {buffer.current.chunks.length ? (
            buffer.current.chunks.map((chunk) => (
              <span
                key={chunk.id}
                data-stream={chunk.stream}
                className={chunk.stream === "stderr" ? "output-stderr" : ""}
              >
                {chunk.text}
              </span>
            ))
          ) : (
            <span className="muted">{t("output.empty")}</span>
          )}
        </pre>
        <div className="output-footer">
          <span>{outputStatus}</span>
          {buffer.current.truncated && <span>{t("output.recentOnly")}</span>}
          {!follow && (
            <button className="text-button" onClick={() => setFollow(true)}>
              {t("output.jumpToLatest")}
            </button>
          )}
        </div>
      </section>
      {error && (
        <ErrorNotice
          title={t("output.failed")}
          detail={error}
          retry={retry}
          dismiss={clearError}
        />
      )}
    </section>
  );
}
