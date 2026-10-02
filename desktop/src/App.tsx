import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type Action,
  type Confirm,
  type Device,
  type Status,
} from "./api";
import { Icon, IconDefinitions, type IconName } from "./components/Icon";
import { serviceLabel } from "./format";
import { Overview } from "./pages/Overview";
import { Devices } from "./pages/Devices";
import { SettingsPage } from "./pages/Settings";
import { History } from "./pages/History";

type Page = "overview" | "devices" | "history" | "settings";
const pages: { id: Page; label: string; icon: IconName }[] = [
  { id: "overview", label: "本机状态", icon: "monitor" },
  { id: "devices", label: "设备", icon: "devices" },
  { id: "history", label: "任务与日志", icon: "terminal" },
  { id: "settings", label: "设置", icon: "settings" },
];

export function App() {
  const [page, setPage] = useState<Page>("overview");
  const [status, setStatus] = useState<Status | null>(null);
  const [devices, setDevices] = useState<Device[]>([]);
  const [devicesMessage, setDevicesMessage] = useState("正在读取设备列表…");
  const [devicesLoading, setDevicesLoading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [confirmation, setConfirmation] = useState<{
    title: string;
    message: string;
  } | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const main = useRef<HTMLElement>(null);
  const confirmResolve = useRef<((result: boolean) => void) | null>(null);
  const busyRef = useRef(false);
  const statusLoading = useRef(false);
  const devicesRequest = useRef(0);
  const reportError = useCallback(
    (value: unknown) => setError(String(value)),
    [],
  );

  const refreshStatus = useCallback(async () => {
    if (statusLoading.current) return null;
    statusLoading.current = true;
    try {
      const value = await api.status();
      setStatus(value);
      if (value.error) setError(value.error);
      return value;
    } catch (e) {
      setError(String(e));
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
      setDevicesMessage(
        "暂时没有其他设备。可以用 xrun invite 邀请新设备加入。",
      );
    } catch (e) {
      if (request === devicesRequest.current)
        setDevicesMessage(`无法查询设备：${e}`);
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

  const confirm: Confirm = (title, message) => {
    if (confirmResolve.current) return Promise.resolve(false);
    setConfirmation({ title, message });
    return new Promise((resolve) => {
      confirmResolve.current = resolve;
    });
  };

  const action: Action = async (request) => {
    if (busyRef.current) return false;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    let success = false;
    try {
      await api.action(request);
      success = true;
    } catch (e) {
      setError(String(e));
    } finally {
      await refreshStatus();
      busyRef.current = false;
      setBusy(false);
    }
    return success;
  };

  const stop = async () => {
    if (
      await confirm(
        "停止后台服务？",
        "这会终止本机上由 xrun 运行的任务。其他设备将无法访问本机，直到你再次启动服务。",
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
  const online = status?.local.daemon_connected === true;

  return (
    <>
      <IconDefinitions />
      <aside className="sidebar">
        <div className="brand">
          <img src="/icon.png" alt="" />
          <span>xrun</span>
        </div>
        <span className="nav-caption">工作空间</span>
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
              {item.id === "devices" && others.length > 0 && (
                <span className="nav-count">{others.length}</span>
              )}
            </button>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <div className="sidebar-device">
            <span className={`dot ${online ? "online" : ""}`} />
            <div>
              <strong>{status?.local.name || "本机设备"}</strong>
              <span>{badge}</span>
            </div>
          </div>
          <span className="version">
            {status && `v${status.local.version}`}
          </span>
        </div>
      </aside>
      <div className="workspace">
        <div className="topbar">
          <span>{pages.find((item) => item.id === page)?.label}</span>
          <span className="topbar-label">个人工作空间</span>
        </div>
        <main ref={main}>
          <section
            id="page-overview"
            className="page"
            hidden={page !== "overview"}
          >
            <Overview
              status={status}
              busy={busy}
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
              status={status}
              devices={others}
              busy={busy}
              loading={devicesLoading}
              message={
                status?.local.joined
                  ? devicesMessage
                  : "先加入部署，就能在这里查看其他设备。"
              }
              refresh={refreshDevices}
              action={action}
              confirm={confirm}
              notify={setToast}
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
              action={action}
              stop={stop}
              confirm={confirm}
              notify={setToast}
              onError={reportError}
            />
          </section>
        </main>
        {error && (
          <div id="error" className="error" role="alert">
            {error}
          </div>
        )}
        {toast && (
          <div id="toast" className="toast" role="status">
            {toast}
          </div>
        )}
      </div>
      <dialog
        ref={dialog}
        id="confirm"
        onClose={() => {
          confirmResolve.current?.(dialog.current?.returnValue === "ok");
          confirmResolve.current = null;
          setConfirmation(null);
        }}
      >
        <form method="dialog">
          <div className="dialog-icon">
            <Icon name="shield" />
          </div>
          <h2>{confirmation?.title}</h2>
          <p>{confirmation?.message}</p>
          <div className="dialog-actions">
            <button value="cancel">取消</button>
            <button className="primary" value="ok">
              确认
            </button>
          </div>
        </form>
      </dialog>
    </>
  );
}
