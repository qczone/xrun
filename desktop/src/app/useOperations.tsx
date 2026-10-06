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
export const operationLabels: Record<PendingOperation, string> = {
  start: "正在启动后台服务…",
  stop: "正在停止后台服务…",
  remove_service: "正在移除后台服务…",
  hide_icon: "正在隐藏图标…",
  join: "正在加入网络…",
  create_network: "正在创建网络…",
  autostart: "正在保存登录启动设置…",
  permission: "正在更新访问权限…",
  all_permissions: "正在保存全体授权…",
  pause_access: "正在更新远程访问…",
  save_settings: "正在保存执行环境…",
  invite: "正在生成邀请…",
  copy_invitation: "正在复制邀请链接…",
  revoke: "正在撤销成员身份…",
};

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
    title: string;
    detail: string;
  } | null>(null);
  const [toast, setToast] = useState<string | null>(null);
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
            ? "网络已创建，后台服务未启动。请启动后台服务。"
            : "已加入网络，后台服务未启动。请启动后台服务。"
          : context.title;
      setError({ page: origin, title, detail });
    } finally {
      await refreshStatus();
      activity.current.busy = false;
      setPending(null);
    }
  };
  const action: Action = async (request) => {
    const titles: Record<typeof request.command, string> = {
      start: "后台服务未能启动，请查看详情后重试。",
      stop: "后台服务未能停止，请重试。",
      remove_service: "后台服务未能移除，请重试。",
      hide_icon: "图标未能隐藏，请重试。",
      join: "未能加入网络，请检查邀请链接和网络连接。",
      create_network: "网络创建未完成，请检查中转部署链接后重试。",
      autostart: "登录启动设置未能保存，请重试。",
      permission: "访问权限未能更新，请重试。",
      all_permissions: "全体授权设置未能保存，请重试。",
      pause_access: "远程访问设置未能更新，请重试。",
      save_settings: "执行环境未能保存，请检查输入后重试。",
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
      if (request.command === "start") setToast("后台服务已启动");
      if (request.command === "stop") setToast("后台服务已停止");
      if (request.command === "remove_service")
        setToast("后台服务已移除，设备身份和记录已保留");
      if (request.command === "pause_access")
        setToast(request.args.paused ? "远程访问已暂停" : "远程访问已恢复");
      if (request.command === "autostart") setToast("登录启动设置已保存");
    }
    return success;
  };

  const stop = async () => {
    if (
      await confirm(
        "停止后台服务？",
        "这会终止本机上由 xrun 运行的任务。其他设备将无法访问本机，直到你再次启动服务。",
        { label: "停止服务", tone: "danger" },
      )
    )
      await action({ command: "stop" });
  };

  const reportError = (value: unknown, title = "无法读取本机设置") =>
    setError({ page: "settings", title, detail: errorText(value) });
  return {
    busy: pending !== null,
    pending,
    error,
    toast,
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
