import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import { api, type Status } from "../api";
import { errorText } from "../errors";
import type { Activity } from "./useOperations";

export function useStatus(activity: RefObject<Activity>) {
  const [status, setStatus] = useState<Status | null>(null);
  const [error, setError] = useState<string | null>(null);
  const request = useRef(0);
  const loading = useRef(false);
  const dismissed = useRef<string | null>(null);
  const refresh = useCallback(async () => {
    const token = ++request.current;
    loading.current = true;
    try {
      const value = await api.status();
      if (token !== request.current) return null;
      setStatus(value);
      if (!value.error) dismissed.current = null;
      const detail = value.error ? errorText(value.error) : null;
      setError(detail === dismissed.current ? null : detail);
      return value;
    } catch (failure) {
      const detail = errorText(failure);
      if (token === request.current && detail !== dismissed.current)
        setError(detail);
      return null;
    } finally {
      if (token === request.current) loading.current = false;
    }
  }, []);
  useEffect(() => {
    void refresh();
    const timer = setInterval(() => {
      if (
        !loading.current &&
        !activity.current.busy &&
        !activity.current.confirming
      )
        void refresh();
    }, 3000);
    return () => {
      request.current++;
      loading.current = false;
      clearInterval(timer);
    };
  }, [activity, refresh]);
  const dismiss = useCallback((detail: string) => {
    dismissed.current = detail;
    setError(null);
  }, []);
  const clearError = useCallback(() => setError(null), []);
  return { status, error, refresh, dismiss, clearError };
}
