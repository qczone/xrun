import { useCallback, useEffect, useRef, useState } from "react";
import { api, type FileRecord, type Job, type TaskFilter } from "../../api";
import { errorText } from "../../errors";
export function useRecordHistory(
  active: boolean,
  paused: boolean,
  selected: boolean,
) {
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
  const [loading, setLoading] = useState(false);
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

  const reset = useCallback(() => {
    setPagination({ cursors: [null], index: 0 });
    setNext(null);
    setError(null);
  }, []);
  const selectTab = (value: "tasks" | "files") => {
    setTab(value);
    reset();
  };
  const selectFilter = (value: TaskFilter) => {
    setFilter(value);
    reset();
  };
  const previousPage = () =>
    setPagination((value) => ({ ...value, index: value.index - 1 }));
  const nextPage = () => {
    if (next !== null)
      setPagination((value) => ({
        index: value.index + 1,
        cursors: [...value.cursors.slice(0, value.index + 1), next],
      }));
  };
  return {
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
    selectTab,
    selectFilter,
    previousPage,
    nextPage,
    clearLoading: () => setLoading(false),
    clearError: () => setError(null),
    refreshRecords: () => setRefresh((value) => value + 1),
  };
}
