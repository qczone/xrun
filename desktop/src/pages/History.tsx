import { useCallback, useState } from "react";
import type { Device, Status, TaskFilter } from "../api";
import { Icon } from "../components/Icon";
import { ErrorNotice } from "../components/ErrorNotice";
import { commandText, duration, fileSize, recordTime } from "../format";
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
                  history.selectTab("tasks");
                  setSelected(null);
                }}
              >
                执行任务
              </button>
              <button
                aria-pressed={tab === "files"}
                className={tab === "files" ? "selected" : ""}
                onClick={() => {
                  history.selectTab("files");
                  setSelected(null);
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
                    history.selectFilter(event.target.value as TaskFilter);
                    setSelected(null);
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
              onClick={history.previousPage}
            >
              上一页
            </button>
            <span className="muted">第 {pagination.index + 1} 页</span>
            <button
              disabled={loading || next === null || selected !== null}
              onClick={history.nextPage}
            >
              下一页
            </button>
          </div>
          {error && (
            <ErrorNotice
              title="活动记录未能读取，请重试。"
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
      <p className="footnote">
        任务结果会保留，已结束任务的输出和文件操作记录按现有规则保留 7 天。
      </p>
    </>
  );
}
