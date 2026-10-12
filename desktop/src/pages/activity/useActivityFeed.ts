import { useCallback, useEffect, useRef, useState } from "react";
import { api, type Job, type ActivityFilter } from "../../api";
import { errorText } from "../../errors";
export function useActivityFeed(
  active: boolean,
  paused: boolean,
  selected: boolean,
) {
  const [filter, setFilter] = useState<ActivityFilter>("all");
  const [pagination, setPagination] = useState<{
    cursors: (string | null)[];
    index: number;
  }>({ cursors: [null], index: 0 });
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [jobs, setJobs] = useState<Job[]>([]);
  const [dbId, setDbId] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [refreshVersion, setRefreshVersion] = useState(0);
  const request = useRef(0);
  const loadedQuery = useRef<string | null>(null);
  const cursor = pagination.cursors[pagination.index] ?? null;
  const query = JSON.stringify([filter, cursor]);

  useEffect(() => {
    if (!active || paused || selected) return;
    let disposed = false;
    let pending = false;
    const load = async () => {
      if (pending || disposed) return;
      pending = true;
      const token = ++request.current;
      setLoading(loadedQuery.current !== query);
      setRefreshing(true);
      try {
        const result = await api.activity(cursor, filter);
        if (disposed || token !== request.current) return;
        setJobs(result.entries);
        setDbId(result.db_id);
        setNextCursor(result.next_cursor);
        loadedQuery.current = query;
        setError(null);
      } catch (e) {
        if (!disposed && token === request.current) setError(errorText(e));
      } finally {
        pending = false;
        if (!disposed) {
          setLoading(false);
          setRefreshing(false);
        }
      }
    };
    void load();
    const timer = setInterval(() => void load(), 3000);
    return () => {
      disposed = true;
      request.current++;
      setLoading(false);
      setRefreshing(false);
      clearInterval(timer);
    };
  }, [active, paused, selected, filter, cursor, query, refreshVersion]);

  const reset = useCallback(() => {
    setPagination({ cursors: [null], index: 0 });
    setNextCursor(null);
    setError(null);
  }, []);
  const selectFilter = (value: ActivityFilter) => {
    setFilter(value);
    reset();
  };
  const previousPage = () =>
    setPagination((value) => ({ ...value, index: value.index - 1 }));
  const nextPage = () => {
    if (nextCursor !== null)
      setPagination((value) => ({
        index: value.index + 1,
        cursors: [...value.cursors.slice(0, value.index + 1), nextCursor],
      }));
  };
  return {
    filter,
    pagination,
    nextCursor,
    jobs,
    dbId,
    loading,
    refreshing,
    error,
    refreshVersion,
    selectFilter,
    previousPage,
    nextPage,
    clearLoading: () => {
      setLoading(false);
      setRefreshing(false);
    },
    clearError: () => setError(null),
    refreshActivities: () => setRefreshVersion((value) => value + 1),
  };
}
