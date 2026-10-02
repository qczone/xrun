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
import {
  commandText,
  duration,
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
        if (!disposed && token === request.current) setError(String(e));
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
    setPagination({ cursors: [null], index: 0 });
    setNext(null);
    setError(null);
  };
  const records = tab === "tasks" ? jobs : files;

  return (
    <>
      <div className="page-heading">
        <div>
          <h1>任务与日志</h1>
          <p>查看在本机执行的任务与文件操作，服务停止后也可查询。</p>
        </div>
        <button
          disabled={selected ? outputLoading : loading}
          onClick={() => setRefresh((value) => value + 1)}
        >
          <Icon name="refresh" />
          刷新
        </button>
      </div>
      {selected ? (
        <TaskDetail
          key={`${selected.dbId}:${selected.job.job_id}`}
          selection={selected}
          active={active}
          paused={paused}
          refresh={refresh}
          status={status}
          deviceLabel={deviceLabel}
          onLoading={setOutputLoading}
          back={() => {
            setSelected(null);
            setError(null);
          }}
        />
      ) : (
        <div>
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
          <section className="panel">
            <div id="history-records">
              {tab === "tasks"
                ? jobs.map((job) => (
                    <button
                      className="task-record"
                      key={job.job_id}
                      onClick={() => {
                        setLoading(false);
                        setError(null);
                        setSelected({ job, dbId: dbId || job.db_id });
                      }}
                    >
                      <span className="record-header">
                        <strong className="mono">{job.job_id}</strong>
                        <StateChip job={job} />
                        <span className="record-time">
                          {recordTime(job.created_at_ms)}
                        </span>
                      </span>
                      <span className="record-command mono">
                        {commandText(job)}
                      </span>
                      <span className="record-meta">
                        来源：{deviceLabel(job.source_device_id)} ·{" "}
                        {job.duration_ms === null
                          ? "点击查看输出"
                          : `耗时 ${duration(job.duration_ms)}`}
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
                        <span className="record-time">
                          {recordTime(record.time_ms)}
                        </span>
                      </div>
                      {record.path && (
                        <p className="record-command mono">{record.path}</p>
                      )}
                      <p className="record-meta">
                        来源：{deviceLabel(record.source_device_id)}
                        {fileSize(record.size)}
                      </p>
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
              disabled={loading || pagination.index === 0}
              onClick={() =>
                setPagination((value) => ({ ...value, index: value.index - 1 }))
              }
            >
              上一页
            </button>
            <span className="muted">第 {pagination.index + 1} 页</span>
            <button
              disabled={loading || next === null}
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
          <p className="footnote">
            任务结果会保留，已结束任务的输出和文件操作记录按现有规则保留 7 天。
          </p>
        </div>
      )}
      {error && (
        <p className="history-error" role="alert">
          {error}
        </p>
      )}
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
          setError(String(e));
          setOutputStatus("输出读取失败");
          if (/^(DB_RESET|JOB_NOT_FOUND|DB_MISSING)/.test(String(e)))
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
    if (follow && log.current) log.current.scrollTop = log.current.scrollHeight;
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

  return (
    <section id="task-detail">
      <button className="history-back" onClick={back}>
        ← 返回任务列表
      </button>
      <article className="panel padded task-heading">
        <div>
          <h2 className="mono">任务 {job.job_id}</h2>
          <StateChip job={job} />
        </div>
        <p className="mono">{commandText(job)}</p>
      </article>
      <section className="panel task-facts">
        <div className="panel-row">
          <span>来源设备</span>
          <span>
            {source === job.source_device_id
              ? source
              : `${source} · ${job.source_device_id}`}
          </span>
        </div>
        <div className="panel-row">
          <span>工作目录</span>
          <code>{job.cwd}</code>
        </div>
        <div className="panel-row">
          <span>开始时间</span>
          <span>{recordTime(job.created_at_ms)}</span>
        </div>
        <div className="panel-row">
          <span>运行耗时</span>
          <span>
            {job.duration_ms !== null
              ? duration(job.duration_ms)
              : isRunning(job)
                ? `约 ${duration(Math.max(0, Date.now() - job.created_at_ms))}`
                : "—"}
          </span>
        </div>
        <div className="panel-row">
          <span>退出结果</span>
          <span>
            {job.signal
              ? `信号 ${job.signal}`
              : job.exit_code !== null
                ? `退出码 ${job.exit_code}`
                : isRunning(job)
                  ? "尚未结束"
                  : "无退出码"}
          </span>
        </div>
      </section>
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
            <option value="all">stdout + stderr</option>
            <option value="stdout">stdout</option>
            <option value="stderr">stderr</option>
          </select>
          <label className="follow-output">
            <input
              type="checkbox"
              checked={follow}
              onChange={(event) => setFollow(event.target.checked)}
            />
            跟随输出
          </label>
        </div>
        <pre
          id="task-log"
          ref={log}
          className="task-log"
          data-filter={stream}
          tabIndex={0}
          aria-label="任务输出"
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
        </div>
      </section>
      {error && (
        <p className="history-error" role="alert">
          {error}
        </p>
      )}
    </section>
  );
}
