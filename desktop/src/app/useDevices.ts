import { t, type PlainMessageKey } from "../i18n";
import { useCallback, useEffect, useRef, useState } from "react";
import { api, type Device, type Revocation, type Status } from "../api";
import { errorText } from "../errors";

export function useDevices(
  status: Status | null,
  visible: boolean,
  paused: boolean,
) {
  const scope = status?.local.joined
    ? status.network?.network_id || status.local.device_id || "joined"
    : null;
  const currentScope = useRef(scope);
  currentScope.current = scope;
  const [devices, setDevices] = useState<Device[]>([]);
  const [message, setMessage] = useState<PlainMessageKey>("devices.reading");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const request = useRef(0);
  const refresh = useCallback(async () => {
    if (!scope || scope !== currentScope.current) return;
    const token = ++request.current;
    setLoading(true);
    setMessage("devices.querying");
    try {
      const result = await api.devices();
      if (token !== request.current || scope !== currentScope.current) return;
      if (result.server_error) throw result.server_error;
      setDevices(
        (result.devices || []).sort(
          (left, right) =>
            Number(right.online) - Number(left.online) ||
            left.name.localeCompare(right.name),
        ),
      );
      setError(null);
      setMessage("devices.empty");
    } catch (failure) {
      if (token === request.current && scope === currentScope.current) {
        setError(errorText(failure));
        setMessage("devices.unavailable");
      }
    } finally {
      if (token === request.current) setLoading(false);
    }
  }, [scope]);
  useEffect(() => {
    request.current++;
    setDevices([]);
    setError(null);
    setLoading(false);
    setMessage(scope ? "devices.reading" : "devices.joinFirst");
    void refresh();
    return () => {
      request.current++;
    };
  }, [scope, refresh]);
  useEffect(() => {
    if (!visible || paused || !scope) return;
    const timer = setInterval(() => {
      if (!loading) void refresh();
    }, 3000);
    return () => clearInterval(timer);
  }, [visible, paused, scope, loading, refresh]);
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
    error,
    refresh,
    applyRevocation,
    scope,
  };
}
