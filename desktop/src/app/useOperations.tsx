import { t, type PlainMessageKey } from "../i18n";
import {
  createContext,
  useContext,
  useEffect,
  useState,
  type RefObject,
} from "react";
import {
  api,
  type Action,
  type Confirm,
  type Operation,
  type PendingOperation,
} from "../api";
import { errorCode, errorText } from "../errors";
export type Page = "overview" | "devices" | "history" | "settings";
export interface Activity {
  busy: boolean;
  confirming: boolean;
}
export function useOperationController(
  page: Page,
  refreshStatus: () => Promise<unknown>,
  confirm: Confirm,
  activity: RefObject<Activity>,
  clearStatusError: () => void,
) {
  const [pending, setPending] = useState<PendingOperation | null>(null);
  const [error, setError] = useState<{
    page: Page;
    title: PlainMessageKey;
    detail: string;
  } | null>(null);
  const [toast, setToast] = useState<PlainMessageKey | null>(null);
  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(null), 2600);
    return () => clearTimeout(timer);
  }, [toast]);
  const operate: Operation = async (operation, context) => {
    if (activity.current.busy) return undefined;
    activity.current.busy = true;
    const origin = page;
    setPending(context.name);
    setError(null);
    clearStatusError();
    try {
      return await operation();
    } catch (e) {
      const detail = errorText(e);
      const title =
        errorCode(e) === "SERVICE_START_FAILED"
          ? context.name === "create_network"
            ? "action.createdServiceFailed"
            : "action.joinedServiceFailed"
          : context.title;
      setError({ page: origin, title, detail });
    } finally {
      await refreshStatus();
      activity.current.busy = false;
      setPending(null);
    }
  };
  const action: Action = async (request) => {
    const titles: Record<typeof request.command, PlainMessageKey> = {
      start: "action.startFailed",
      stop: "action.stopFailed",
      remove_service: "action.removeFailed",
      hide_icon: "action.hideFailed",
      join: "action.joinFailed",
      create_network: "action.createFailed",
      autostart: "action.autostartFailed",
      permission: "action.permissionFailed",
      all_permissions: "action.allPermissionsFailed",
      pause_access: "action.pauseFailed",
      save_settings: "action.settingsFailed",
      save_attachment_retention: "settings.retentionFailed",
    };
    const success =
      (await operate(
        async () => {
          await api.action(request);
          return true;
        },
        { name: request.command, title: titles[request.command] },
      )) === true;
    if (success) {
      if (request.command === "start") setToast("action.started");
      if (request.command === "stop") setToast("action.stopped");
      if (request.command === "remove_service") setToast("action.removed");
      if (request.command === "pause_access")
        setToast(request.args.paused ? "action.paused" : "action.resumed");
      if (request.command === "autostart") setToast("action.autostartSaved");
    }
    return success;
  };

  const stop = async () => {
    if (
      await confirm(t("service.stopTitle"), t("service.stopMessage"), {
        label: t("service.stop"),
        tone: "danger",
      })
    )
      await action({ command: "stop" });
  };

  const reportError = (
    value: unknown,
    title: PlainMessageKey = "error.settings",
  ) => setError({ page: "settings", title, detail: errorText(value) });
  return {
    busy: pending !== null,
    pending,
    error: error && { ...error, title: t(error.title) },
    toast: toast && t(toast),
    notify: setToast,
    clearError: () => setError(null),
    operate,
    action,
    stop,
    confirm,
    reportError,
  };
}
export const OperationsContext = createContext<ReturnType<
  typeof useOperationController
> | null>(null);
export function useOperations() {
  const operations = useContext(OperationsContext);
  if (!operations) throw new Error("Operation provider is missing");
  return operations;
}
