import { errorCode } from "../errors";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type Device,
  type FileRecord,
  type Job,
  type Status,
  type TaskFilter,
} from "../api";
import { Icon } from "../components/Icon";
import { ErrorNotice } from "../components/ErrorNotice";
import {
  commandText,
  duration,
  errorText,
  fileSize,
  isRunning,
  recordTime,
  taskState,
} from "../format";
import { OutputBuffer } from "../logs";

interface Props {
  active: boolean;
  paused: boolean;
  status: Status | null;
  devices: Device[];
}
type Selection = { job: Job; dbId: string };

function StateChip({ job }: { job: Job }) {
  const [label, kind] = taskState(job);
  return <span className={`task-state ${kind}`}>{label}</span>;
}

export function History({ active, paused, status, devices }: Props) {
  const [tab, setTab] = useState<"tasks" | "files">("tasks");
  const [filter, setFilter] = useState<TaskFilter>("all");
  const [pagination, setPagination] = useState<{
    cursors: (number | null)[];
    index: number;
  }>({ cursors: [null], index: 0 });
  const [next, setNext] = useState<number | null>(null);
  const [jobs, setJobs] = useState<Job[]>([]);
  const [files, setFiles] = useState<FileRecord[]>([]);
  const [dbId, setDbId] = useState<string | null>(null);
  const [selected, setSelected] = useState<Selection | null>(null);
  const [loading, setLoading] = useState(false);
  const [outputLoading, setOutputLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const request = useRef(0);
  const cursor = pagination.cursors[pagination.index] ?? null;

  useEffect(() => {
    if (!active || paused || selected) return;
    let disposed = false;
    let pending = false;
    const load = async () => {
      if (pending || disposed) return;
      pending = true;
      const token = ++request.current;
      setLoading(true);
      try {
        if (tab === "tasks") {
          const result = await api.tasks(cursor, filter);
          if (disposed || token !== request.current) return;
          setJobs(result.jobs);
          setDbId(result.db_id);
          setNext(result.next_cursor);
        } else {
          const result = await api.files(cursor);
          if (disposed || token !== request.current) return;
          setFiles(result.entries);
          setNext(result.next_cursor);
        }
        setError(null);
      } catch (e) {
        if (!disposed && token === request.current) setError(errorText(e));
      } finally {
        pending = false;
        if (!disposed) setLoading(false);
      }
    };
    void load();
    const timer = setInterval(() => void load(), 3000);
    return () => {
      disposed = true;
      request.current++;
      clearInterval(timer);
    };
  }, [active, paused, selected, tab, filter, cursor, refresh]);

  const deviceLabel = useCallback(
    (id: string) => {
      if (id === status?.local.device_id) return status.local.name || id;
      return devices.find((device) => device.device_id === id)?.name || id;
    },
    [status, devices],
  );
  const reset = () => {
    setSelected(null);
    setPagination({ cursors: [null], index: 0 });
    setNext(null);
    setError(null);
  };
  const refreshRecords = () => setRefresh((value) => value + 1);
  const records = tab === "tasks" ? jobs : files;

  return (
    <>
      <div className="page-heading">
        <div>
          <h1>本机活动记录</h1>
          <p>在这台设备执行的任务、文件操作和截图；后台服务停止后也可查看。</p>
        </div>
        <button
          disabled={selected ? outputLoading : loading}
          onClick={refreshRecords}
        >
          <Icon name="refresh" />
          {(selected ? outputLoading : loading) ? "正在刷新…" : "刷新"}
        </button>
      </div>
      <div className={`history-layout ${selected ? "has-selection" : ""}`}>
        <div className="history-list">
          <div className="history-toolbar">
            <div className="segmented" role="group" aria-label="记录类型">
              <button
                aria-pressed={tab === "tasks"}
                className={tab === "tasks" ? "selected" : ""}
                onClick={() => {
                  setTab("tasks");
                  reset();
                }}
              >
                执行任务
              </button>
              <button
                aria-pressed={tab === "files"}
                className={tab === "files" ? "selected" : ""}
                onClick={() => {
                  setTab("files");
                  reset();
                }}
              >
                文件与截图
              </button>
            </div>
            {tab === "tasks" && (
              <label>
                状态
                <select
                  aria-label="任务状态"
                  value={filter}
                  onChange={(event) => {
                    setFilter(event.target.value as TaskFilter);
                    reset();
                  }}
                >
                  <option value="all">全部任务</option>
                  <option value="running">正在运行</option>
                  <option value="failed">异常结束</option>
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
                        setLoading(false);
                        setError(null);
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
                        来源：{deviceLabel(job.source_device_id)} ·{" "}
                        {job.duration_ms === null
                          ? "点击查看输出"
                          : `耗时 ${duration(job.duration_ms)}`}
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
                              push: "接收文件",
                              pull: "发送文件",
                              screenshot: "截图",
                            } as Record<string, string>
                          )[record.op] || record.op}
                        </strong>
                        <span
                          className={`task-state ${record.result === "ok" ? "success" : "failed"}`}
                        >
                          {record.result === "ok" ? "已完成" : "失败或中断"}
                        </span>
                      </div>
                      {record.path && (
                        <p className="record-command mono">{record.path}</p>
                      )}
                      <p className="record-meta">
                        来源：{deviceLabel(record.source_device_id)}
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
                  ? "正在读取记录…"
                  : tab === "files"
                    ? "暂无文件操作或截图记录。"
                    : filter === "all"
                      ? "暂无任务。通过 xrun 在本机执行的任务会出现在这里。"
                      : "暂无符合状态的任务。"}
              </p>
            )}
          </section>
          <div className="history-pagination">
            <button
              disabled={loading || pagination.index === 0 || selected !== null}
              onClick={() =>
                setPagination((value) => ({ ...value, index: value.index - 1 }))
              }
            >
              上一页
            </button>
            <span className="muted">第 {pagination.index + 1} 页</span>
            <button
              disabled={loading || next === null || selected !== null}
              onClick={() => {
                if (next !== null)
                  setPagination((value) => ({
                    index: value.index + 1,
                    cursors: [...value.cursors.slice(0, value.index + 1), next],
                  }));
              }}
            >
              下一页
            </button>
          </div>
          {error && (
            <ErrorNotice
              title="活动记录未能读取，请重试。"
              detail={error}
              retry={refreshRecords}
              dismiss={() => setError(null)}
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
              setError(null);
            }}
          />
        )}
      </div>
      <p className="footnote">
        任务结果会保留，已结束任务的输出和文件操作记录按现有规则保留 7 天。
      </p>
    </>
  );
}

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

function TaskDetail({
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
  const [job, setJob] = useState(selection.job);
  const [stream, setStream] = useState("all");
  const [follow, setFollow] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [outputStatus, setOutputStatus] = useState("正在读取输出…");
  const [revision, setRevision] = useState(0);
  const buffer = useRef(new OutputBuffer());
  const finished = useRef(false);
  const log = useRef<HTMLPreElement>(null);
  const lastScrollTop = useRef(0);
  const {
    dbId,
    job: { job_id: jobId },
  } = selection;

  useEffect(() => {
    if (!active || paused) return;
    let disposed = false;
    let pending = false;
    const load = async (force = false) => {
      if (disposed || pending || (finished.current && !force)) return;
      pending = true;
      onLoading(true);
      let more = false;
      try {
        const result = await api.output(dbId, jobId, buffer.current.after);
        if (disposed) return;
        buffer.current.consume(result);
        setJob(result.job);
        setError(null);
        setRevision((value) => value + 1);
        more = result.has_more;
        finished.current = !isRunning(result.job) && !more;
        setOutputStatus(
          more
            ? "正在补读输出…"
            : finished.current
              ? "任务已结束"
              : "每 3 秒更新输出",
        );
      } catch (e) {
        if (!disposed) {
          setError(errorText(e));
          setOutputStatus("输出读取失败");
          if (
            ["DB_RESET", "JOB_NOT_FOUND", "DB_MISSING"].includes(
              errorCode(e) ?? "",
            )
          )
            finished.current = true;
        }
      } finally {
        pending = false;
        if (!disposed) {
          onLoading(false);
          if (more) queueMicrotask(() => void load());
        }
      }
    };
    // StrictMode's first setup is discarded before issuing an IPC request.
    queueMicrotask(() => void load(true));
    const timer = setInterval(() => void load(), 3000);
    // A late response is ignored after hiding or changing the selected task.
    return () => {
      disposed = true;
      onLoading(false);
      clearInterval(timer);
    };
  }, [active, paused, refresh, dbId, jobId, onLoading]);

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
          dismiss={() => setError(null)}
        />
      )}
    </section>
  );
}
