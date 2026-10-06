import { errorCode } from "./errors";
import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type Action,
  type Confirm,
  type ConfirmOptions,
  type Device,
  type Operation,
  type PendingOperation,
  type Status,
} from "./api";
import { Icon, IconDefinitions, type IconName } from "./components/Icon";
import { ErrorNotice } from "./components/ErrorNotice";
import { errorText, serviceLabel } from "./format";
import { Overview } from "./pages/Overview";
import { Devices } from "./pages/Devices";
import { SettingsPage } from "./pages/Settings";
import { History } from "./pages/History";

type Page = "overview" | "devices" | "history" | "settings";
const pages: { id: Page; label: string; icon: IconName }[] = [
  { id: "overview", label: "本机", icon: "monitor" },
  { id: "devices", label: "设备", icon: "devices" },
  { id: "history", label: "活动记录", icon: "terminal" },
  { id: "settings", label: "设置", icon: "settings" },
];
const operationLabels: Record<PendingOperation, string> = {
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

export function App() {
  const [page, setPage] = useState<Page>("overview");
  const [status, setStatus] = useState<Status | null>(null);
  const [devices, setDevices] = useState<Device[]>([]);
  const [devicesMessage, setDevicesMessage] = useState("正在读取设备列表…");
  const [devicesLoading, setDevicesLoading] = useState(false);
  const [devicesError, setDevicesError] = useState<string | null>(null);
  const [pending, setPending] = useState<PendingOperation | null>(null);
  const busy = pending !== null;
  const [statusError, setStatusError] = useState<string | null>(null);
  const [error, setError] = useState<{
    page: Page;
    title: string;
    detail: string;
  } | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<{
    title: string;
    message: string;
    options: ConfirmOptions;
  } | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const main = useRef<HTMLElement>(null);
  const confirmResolve = useRef<((result: boolean) => void) | null>(null);
  const busyRef = useRef(false);
  const statusLoading = useRef(false);
  const dismissedStatusError = useRef<string | null>(null);
  const devicesRequest = useRef(0);
  const reportError = useCallback(
    (value: unknown, title = "无法读取本机设置") =>
      setError({ page: "settings", title, detail: errorText(value) }),
    [],
  );

  const refreshStatus = useCallback(async () => {
    if (statusLoading.current) return null;
    statusLoading.current = true;
    try {
      const value = await api.status();
      setStatus(value);
      if (!value.error) dismissedStatusError.current = null;
      const detail = value.error ? errorText(value.error) : null;
      setStatusError(detail === dismissedStatusError.current ? null : detail);
      return value;
    } catch (e) {
      const detail = errorText(e);
      if (detail !== dismissedStatusError.current) setStatusError(detail);
      return null;
    } finally {
      statusLoading.current = false;
    }
  }, []);

  const refreshDevices = useCallback(async () => {
    const request = ++devicesRequest.current;
    setDevicesLoading(true);
    setDevicesMessage("正在查询设备…");
    try {
      const result = await api.devices();
      if (request !== devicesRequest.current) return;
      if (result.server_error)
        throw `${result.server_error.code}: ${result.server_error.message}`;
      setDevices(
        (result.devices || []).sort(
          (a, b) =>
            Number(b.online) - Number(a.online) || a.name.localeCompare(b.name),
        ),
      );
      setDevicesError(null);
      setDevicesMessage(
        "暂时没有其他设备。新设备需要管理设备生成的邀请链接才能加入。",
      );
    } catch (e) {
      if (request === devicesRequest.current) {
        setDevicesError(errorText(e));
        setDevicesMessage("设备列表暂时不可用，请刷新重试。");
      }
    } finally {
      if (request === devicesRequest.current) setDevicesLoading(false);
    }
  }, []);

  useEffect(() => {
    void refreshStatus();
    const timer = setInterval(() => {
      if (!busyRef.current && !confirmResolve.current) void refreshStatus();
    }, 3000);
    return () => clearInterval(timer);
  }, [refreshStatus]);

  const joined = !!status?.local.joined;
  useEffect(() => {
    if (joined) void refreshDevices();
  }, [joined, refreshDevices]);

  useEffect(() => {
    if (!toast) return;
    const timer = setTimeout(() => setToast(null), 2600);
    return () => clearTimeout(timer);
  }, [toast]);

  useEffect(() => {
    if (confirmation && dialog.current && !dialog.current.open)
      dialog.current.showModal();
  }, [confirmation]);

  useEffect(
    () => () => {
      confirmResolve.current?.(false);
    },
    [],
  );

  const confirm: Confirm = (title, message, options) => {
    if (confirmResolve.current) return Promise.resolve(false);
    setConfirmation({ title, message, options });
    return new Promise((resolve) => {
      confirmResolve.current = resolve;
    });
  };

  const operate: Operation = async (operation, context) => {
    if (busyRef.current) return undefined;
    busyRef.current = true;
    const origin = page;
    setPending(context.name);
    setError(null);
    setStatusError(null);
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
      busyRef.current = false;
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
  const revokeDevice = (device: string) =>
    operate(
      async () => {
        const result = await api.revoke(device);
        if (!result.revoked) throw "撤销结果未确认，请刷新后重试。";
        setDevices((current) =>
          current.map((entry) =>
            entry.device_id === result.device_id
              ? { ...entry, revoked: true, online: false }
              : entry,
          ),
        );
        await refreshDevices();
        return result;
      },
      { name: "revoke", title: "成员身份未能撤销，请重试。" },
    );

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
  const navigate = (next: Page) => {
    setPage(next);
    if (main.current) main.current.scrollTop = 0;
  };
  const others = devices.filter(
    (device) => device.device_id !== status?.local.device_id,
  );
  const [badge] = serviceLabel(status);
  const stateTone =
    status?.local.remote_access_paused || status?.service.approval_required
      ? "warning"
      : status?.local.daemon_connected === true
        ? "online"
        : "";
  const feedback = (origin: Page) =>
    error?.page === origin ? (
      <ErrorNotice
        title={error.title}
        detail={error.detail}
        dismiss={() => {
          dismissedStatusError.current = error.detail;
          setStatusError(null);
          setError(null);
        }}
      />
    ) : null;

  return (
    <>
      <IconDefinitions />
      <aside className="sidebar">
        <div className="brand">
          <img src="/icon.png" alt="" />
          <span>xrun</span>
        </div>
        <nav aria-label="主要导航">
          {pages.map((item) => (
            <button
              key={item.id}
              className={`nav-item ${page === item.id ? "selected" : ""}`}
              data-page={item.id}
              aria-label={item.label}
              aria-current={page === item.id ? "page" : undefined}
              onClick={() => navigate(item.id)}
            >
              <Icon name={item.icon} />
              {item.label}
              {item.id === "devices" &&
                others.some((device) => !device.revoked) && (
                  <span className="nav-count">
                    {others.filter((device) => !device.revoked).length}
                  </span>
                )}
            </button>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <div className="sidebar-device">
            <span className={`dot ${stateTone}`} />
            <div>
              <strong>{status?.local.name || "本机设备"}</strong>
              <span>{badge}</span>
            </div>
          </div>
          {pending && (
            <span className="operation-progress" role="status">
              {operationLabels[pending]}
            </span>
          )}
          <span className="version">
            {status && `v${status.local.version}`}
          </span>
        </div>
      </aside>
      <div className="workspace">
        <main ref={main} aria-busy={busy}>
          {statusError && statusError !== error?.detail && (
            <ErrorNotice
              title="本机检查遇到问题，请查看详情。"
              detail={statusError}
              retry={() => void refreshStatus()}
              dismiss={() => {
                dismissedStatusError.current = statusError;
                setStatusError(null);
              }}
            />
          )}
          <section
            id="page-overview"
            className="page"
            hidden={page !== "overview"}
          >
            <Overview
              status={status}
              busy={busy}
              pending={pending}
              feedback={feedback("overview")}
              action={action}
              stop={stop}
              navigate={navigate}
            />
          </section>
          <section
            id="page-devices"
            className="page"
            hidden={page !== "devices"}
          >
            <Devices
              active={page === "devices"}
              status={status}
              devices={others}
              busy={busy}
              pending={pending}
              feedback={feedback("devices")}
              loading={devicesLoading}
              listError={devicesError}
              message={
                status?.local.joined
                  ? devicesMessage
                  : "先创建或加入网络，就能在这里查看其他设备。"
              }
              refresh={refreshDevices}
              action={action}
              confirm={confirm}
              notify={setToast}
              operate={operate}
              revoke={revokeDevice}
            />
          </section>
          <section
            id="page-history"
            className="page"
            hidden={page !== "history"}
          >
            <History
              active={page === "history"}
              paused={confirmation !== null}
              status={status}
              devices={devices}
            />
          </section>
          <section
            id="page-settings"
            className="page"
            hidden={page !== "settings"}
          >
            <SettingsPage
              status={status}
              busy={busy}
              pending={pending}
              feedback={feedback("settings")}
              action={action}
              stop={stop}
              confirm={confirm}
              notify={setToast}
              onError={reportError}
            />
          </section>
        </main>
        {toast && (
          <div id="toast" className="toast" role="status">
            {toast}
          </div>
        )}
      </div>
      <dialog
        ref={dialog}
        id="confirm"
        aria-labelledby="confirm-title"
        onClose={() => {
          confirmResolve.current?.(dialog.current?.returnValue === "ok");
          confirmResolve.current = null;
          setConfirmation(null);
        }}
      >
        <form method="dialog">
          <div
            className={`dialog-icon ${confirmation?.options.tone === "danger" ? "danger" : ""}`}
          >
            <Icon name="shield" />
          </div>
          <h2 id="confirm-title">{confirmation?.title}</h2>
          <p>{confirmation?.message}</p>
          <div className="dialog-actions">
            <button value="cancel">取消</button>
            <button
              className={
                confirmation?.options.tone === "danger"
                  ? "danger-primary"
                  : "primary"
              }
              value="ok"
            >
              {confirmation?.options.label}
            </button>
          </div>
        </form>
      </dialog>
    </>
  );
}
