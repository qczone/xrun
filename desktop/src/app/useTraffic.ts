import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type Status,
  type TrafficPeriod,
  type TrafficReport,
} from "../api";
import { errorText } from "../errors";

export function useTraffic(status: Status | null, active: boolean) {
  const network = status?.network;
  const device = status?.local.device_id;
  const scope =
    status?.local.joined && network && device
      ? JSON.stringify([
          network.network_id,
          device,
          network.is_manager,
          network.relay_addresses,
        ])
      : null;
  const [period, setPeriod] = useState<TrafficPeriod>("month");
  const [offset, setOffset] = useState(0);
  const [report, setReport] = useState<TrafficReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const generation = useRef(0);
  const current = useRef(scope);
  current.current = scope;
  const pending = useRef(false);
  const refresh = useCallback(async () => {
    if (!scope || current.current !== scope) return;
    const token = ++generation.current;
    pending.current = true;
    setLoading(true);
    try {
      const value = await api.traffic(period, offset);
      if (token !== generation.current || current.current !== scope) return;
      setReport(value);
      setError(null);
    } catch (failure) {
      if (token === generation.current && current.current === scope)
        setError(errorText(failure));
    } finally {
      if (token === generation.current) {
        pending.current = false;
        setLoading(false);
      }
    }
  }, [scope, period, offset]);
  useEffect(() => {
    generation.current++;
    pending.current = false;
    setReport(null);
    setError(null);
    setLoading(false);
    return () => {
      generation.current++;
    };
  }, [scope, period, offset]);
  useEffect(() => {
    if (!scope || !active) return;
    void refresh();
    const timer = setInterval(() => {
      if (!pending.current) void refresh();
    }, 10_000);
    return () => {
      clearInterval(timer);
      generation.current++;
      pending.current = false;
    };
  }, [scope, active, refresh]);
  const changePeriod = (next: TrafficPeriod) => {
    setReport(null);
    setPeriod(next);
    setOffset(0);
  };
  const changeOffset = (next: number) => {
    setReport(null);
    setOffset(next);
  };
  const authorized =
    report?.network_id === network?.network_id &&
    report?.period === period &&
    report?.device_id === (network?.is_manager ? null : device);
  return {
    report: authorized ? report : null,
    period,
    offset,
    changePeriod,
    changeOffset,
    refresh,
    error,
    loading,
  };
}
