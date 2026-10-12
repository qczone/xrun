import { t, type PlainMessageKey } from "../i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type Device, type Revocation, type Status } from "../api";
import { errorText } from "../errors";

export function useDevices(
  status: Status | null,
  visible: boolean,
  paused: boolean,
  windowVisible: boolean,
) {
  const scope = status?.local.joined
    ? status.network?.network_id || status.local.device_id || "joined"
    : null;
  const currentScope = useRef(scope);
  currentScope.current = scope;
  const [devices, setDevices] = useState<Device[]>([]);
  const [message, setMessage] = useState<PlainMessageKey>("devices.reading");
  const [loading, setLoading] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const request = useRef(0);
  const pending = useRef(false);
  const hasResult = useRef(false);
  const refresh = useCallback(
    async (automatic = false) => {
      if (!scope || scope !== currentScope.current) return;
      if (automatic && pending.current) return;
      const token = ++request.current;
      pending.current = true;
      setLoading(!hasResult.current);
      setRefreshing(true);
      try {
        const result = await api.devices();
        if (token !== request.current || scope !== currentScope.current) return;
        if (result.devices !== null) {
          hasResult.current = true;
          setDevices(
            (result.devices || []).sort(
              (left, right) =>
                Number(right.online) - Number(left.online) ||
                left.name.localeCompare(right.name),
            ),
          );
        }
        if (result.server_error) throw result.server_error;
        setError(null);
        setMessage("devices.empty");
      } catch (failure) {
        if (token === request.current && scope === currentScope.current) {
          setError(errorText(failure));
          setMessage("devices.unavailable");
        }
      } finally {
        if (token === request.current) {
          pending.current = false;
          setLoading(false);
          setRefreshing(false);
        }
      }
    },
    [scope],
  );
  useEffect(() => {
    request.current++;
    pending.current = false;
    hasResult.current = false;
    setDevices([]);
    setError(null);
    setLoading(false);
    setRefreshing(false);
    setMessage(scope ? "devices.reading" : "devices.joinFirst");
    return () => {
      request.current++;
    };
  }, [scope, refresh]);
  useEffect(() => {
    if (!windowVisible || paused || !scope) return;
    void refresh();
  }, [windowVisible, paused, scope, refresh]);
  useEffect(() => {
    if (!visible || paused || !scope) return;
    const timer = setInterval(() => {
      void refresh(true);
    }, 3000);
    return () => clearInterval(timer);
  }, [visible, paused, scope, refresh]);
  const applyRevocation = (result: Revocation, network: string | null) => {
    if (network !== currentScope.current) return;
    setDevices((entries) =>
      entries.map((entry) =>
        entry.device_id === result.device_id
          ? { ...entry, revoked: true, online: false }
          : entry,
      ),
    );
  };
  return {
    devices,
    message: t(message),
    loading,
    refreshing,
    error,
    refresh,
    applyRevocation,
    scope,
  };
}
