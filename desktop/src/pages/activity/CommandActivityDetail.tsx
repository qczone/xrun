import { useLayoutEffect, useRef, useState } from "react";
import type { Status } from "../../api";
import { Icon } from "../../components/Icon";
import { Identifier } from "../../components/Identifier";
import { ErrorNotice } from "../../components/ErrorNotice";
import { Select } from "../../components/Select";
import { commandText, duration, isRunning, dateTime } from "../../format";
import { t } from "../../i18n";
import { ActivityState } from "./ActivityState";
import { useCommandOutput, type CommandSelection } from "./useCommandOutput";
interface DetailProps {
  selection: CommandSelection;
  active: boolean;
  paused: boolean;
  refreshVersion: number;
  status: Status | null;
  deviceLabel: (id: string) => string;
  onLoading: (value: boolean) => void;
  retry: () => void;
  onClose: () => void;
}

export function CommandActivityDetail({
  selection,
  active,
  paused,
  refreshVersion,
  status,
  deviceLabel,
  onLoading,
  retry,
  onClose,
}: DetailProps) {
  const { job, error, outputStatus, revision, outputBuffer, clearError } =
    useCommandOutput({ selection, active, paused, refreshVersion, onLoading });
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
    <section id="command-activity-detail">
      <button
        className="activity-close icon-button"
        aria-label={t("activity.closeDetails")}
        onClick={onClose}
      >
        <Icon name="close" />
      </button>
      <article className="panel command-heading">
        <div className="activity-detail-title">
          <h2 className="mono">{commandText(job.params)}</h2>
          <ActivityState job={job} />
        </div>
        <div className="activity-detail-meta">
          <span>{t("activity.source", { name: source })}</span>
          <span>{t("activity.duration", { duration: elapsed })}</span>
          <span>{result}</span>
        </div>
        <details className="activity-detail-facts">
          <summary>{t("activity.technicalDetails")}</summary>
          <div className="panel-row">
            <span>Job ID</span>
            <code>{job.job_id}</code>
          </div>
          <div className="panel-row">
            <span>{t("output.sourceId")}</span>
            <Identifier
              value={job.source_device_id}
              label={t("output.sourceId")}
            />
          </div>
          <div className="panel-row">
            <span>{t("output.cwd")}</span>
            <code>{job.params.cwd}</code>
          </div>
          <div className="panel-row">
            <span>{t("output.startedAt")}</span>
            <span>{dateTime(job.created_at_ms)}</span>
          </div>
        </details>
      </article>
      {!!warnings.length && (
        <p id="command-warning" className="inline-notice" role="status">
          {warnings.join(" ")}
        </p>
      )}
      <section className="panel output-panel">
        <div className="output-toolbar">
          <strong>{t("output.title")}</strong>
          <label className="sr-only" htmlFor="output-stream">
            {t("output.type")}
          </label>
          <Select
            id="output-stream"
            value={stream}
            compact
            onChange={setStream}
            options={[
              { value: "all", label: t("output.all") },
              { value: "stdout", label: t("output.stdout") },
              { value: "stderr", label: t("output.stderr") },
            ]}
          />
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
          id="command-log"
          ref={log}
          className="command-log"
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
          {outputBuffer.current.chunks.length ? (
            outputBuffer.current.chunks.map((chunk) => (
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
          {outputBuffer.current.truncated && (
            <span>{t("output.recentOnly")}</span>
          )}
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
