import { useEffect, useRef, useState } from "react";
import type { Status } from "../../api";
import { ErrorNotice } from "../../components/ErrorNotice";
import { commandText, duration, isRunning, recordTime } from "../../format";
import { StateChip } from "./TaskState";
import { useTaskOutput, type Selection } from "./useTaskOutput";
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
    useTaskOutput({ selection, active, paused, refresh, onLoading });
  const [stream, setStream] = useState("all");
  const [follow, setFollow] = useState(true);
  const log = useRef<HTMLPreElement>(null);
  const lastScrollTop = useRef(0);
  useEffect(() => {
    if (follow && log.current) {
      log.current.scrollTop = log.current.scrollHeight;
      lastScrollTop.current = log.current.scrollTop;
    }
  }, [revision, follow, stream]);

  const source = deviceLabel(job.source_device_id);
  const warnings = [];
  if (job.error) warnings.push(job.error);
  if (!job.output_complete) {
    const reasons: Record<string, string> = {
      TRUNCATED: "输出已被截断。",
      LOG_EXPIRED: "输出已按保留规则清理。",
      DETACHED_OUTPUT: "后台子进程的部分输出未能收集。",
    };
    warnings.push(
      reasons[job.incomplete_reason || ""] ||
        `输出不完整：${job.incomplete_reason || "原因未知"}`,
    );
  }
  if (job.leftover_possible) warnings.push("可能仍有未清理的子进程。");
  if (isRunning(job) && status && !status.local.daemon_running)
    warnings.push("后台服务已停止，记录中的任务状态可能尚未更新。");
  const elapsed =
    job.duration_ms !== null
      ? duration(job.duration_ms)
      : isRunning(job)
        ? `约 ${duration(Math.max(0, Date.now() - job.created_at_ms))}`
        : "—";
  const result = job.signal
    ? `信号 ${job.signal}`
    : job.exit_code !== null
      ? `退出码 ${job.exit_code}`
      : isRunning(job)
        ? "尚未结束"
        : "无退出码";

  return (
    <section id="task-detail">
      <button className="history-back" onClick={back}>
        ← 返回任务列表
      </button>
      <article className="panel task-heading">
        <div className="task-title">
          <h2 className="mono">{commandText(job)}</h2>
          <StateChip job={job} />
        </div>
        <div className="task-summary-meta">
          <span>来源：{source}</span>
          <span>耗时 {elapsed}</span>
          <span>{result}</span>
        </div>
        <details className="task-facts">
          <summary>任务 {job.job_id} · 查看详情</summary>
          <div className="panel-row">
            <span>来源设备 ID</span>
            <code>{job.source_device_id}</code>
          </div>
          <div className="panel-row">
            <span>工作目录</span>
            <code>{job.cwd}</code>
          </div>
          <div className="panel-row">
            <span>开始时间</span>
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
          <strong>执行输出</strong>
          <label className="sr-only" htmlFor="output-stream">
            输出类型
          </label>
          <select
            id="output-stream"
            value={stream}
            onChange={(event) => setStream(event.target.value)}
          >
            <option value="all">全部输出</option>
            <option value="stdout">标准输出（stdout）</option>
            <option value="stderr">错误输出（stderr）</option>
          </select>
          <label className="follow-output">
            <input
              type="checkbox"
              checked={follow}
              onChange={(event) => setFollow(event.target.checked)}
            />
            自动滚动到底部
          </label>
        </div>
        <pre
          id="task-log"
          ref={log}
          className="task-log"
          data-filter={stream}
          tabIndex={0}
          aria-label="任务输出"
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
            <span className="muted">暂无输出</span>
          )}
        </pre>
        <div className="output-footer">
          <span>{outputStatus}</span>
          {buffer.current.truncated && <span>仅显示最近的输出</span>}
          {!follow && (
            <button className="text-button" onClick={() => setFollow(true)}>
              回到最新输出
            </button>
          )}
        </div>
      </section>
      {error && (
        <ErrorNotice
          title="任务输出未能读取，请重试。"
          detail={error}
          retry={retry}
          dismiss={clearError}
        />
      )}
    </section>
  );
}
