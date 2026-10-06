import { useEffect, useRef, useState } from "react";
import { api, type Job } from "../../api";
import { errorCode, errorText } from "../../errors";
import { isRunning } from "../../format";
import { OutputBuffer } from "../../logs";
export type Selection = { job: Job; dbId: string };
interface Input {
  selection: Selection;
  active: boolean;
  paused: boolean;
  refresh: number;
  onLoading: (value: boolean) => void;
}
export function useTaskOutput({
  selection,
  active,
  paused,
  refresh,
  onLoading,
}: Input) {
  const [job, setJob] = useState(selection.job);
  const [error, setError] = useState<string | null>(null);
  const [outputStatus, setOutputStatus] = useState("正在读取输出…");
  const [revision, setRevision] = useState(0);
  const buffer = useRef(new OutputBuffer());
  const finished = useRef(false);
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
            [
              "DB_RESET",
              "JOB_NOT_FOUND",
              "DB_MISSING",
              "DB_SCHEMA_MISMATCH",
              "DB_CORRUPT",
            ].includes(errorCode(e) ?? "")
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

  return {
    job,
    error,
    outputStatus,
    revision,
    buffer,
    clearError: () => setError(null),
  };
}
