import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  api,
  type Action,
  type Confirm,
  type ExecutionSettings,
  type PendingOperation,
  type Settings,
  type Status,
} from "../api";
import { Icon } from "../components/Icon";
import { ErrorNotice } from "../components/ErrorNotice";
import { errorText, osName, serviceState } from "../format";

interface Props {
  status: Status | null;
  busy: boolean;
  pending: PendingOperation | null;
  feedback: ReactNode;
  action: Action;
  stop: () => Promise<void>;
  confirm: Confirm;
  notify: (message: string) => void;
  onError: (error: unknown, title?: string) => void;
}

export function SettingsPage({
  status,
  busy,
  pending,
  feedback,
  action,
  stop,
  confirm,
  notify,
  onError,
}: Props) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [cwd, setCwd] = useState("");
  const [concurrency, setConcurrency] = useState("4");
  const [path, setPath] = useState("");
  const [loadError, setLoadError] = useState<string | null>(null);
  const loadRequest = useRef(0);
  const joined = !!status?.local.joined;
  const loadSettings = useCallback(async () => {
    const request = ++loadRequest.current;
    try {
      const value = await api.settings();
      if (request !== loadRequest.current) return;
      setSettings(value);
      setCwd(value.execution.default_cwd || "");
      setConcurrency(String(value.execution.max_concurrent_jobs));
      setPath(value.execution.path || "");
      setLoadError(null);
    } catch (error) {
      if (request === loadRequest.current) setLoadError(errorText(error));
    }
  }, []);

  useEffect(() => {
    void loadSettings();
    return () => {
      loadRequest.current++;
    };
    // Status polling must not overwrite the user's unsaved inputs.
  }, [joined, loadSettings]);

  const execution: ExecutionSettings = {
    default_cwd: cwd.trim() ? cwd : null,
    max_concurrent_jobs: Number(concurrency),
    path: path.trim() ? path : null,
  };
  const dirty =
    settings !== null &&
    JSON.stringify(execution) !== JSON.stringify(settings.execution);
  const disabled = busy || !joined || !settings;
  const [badge, serviceTone] = serviceState(status);
  const local = status?.local;
  const service = status?.service;

  return (
    <>
      <div className="page-heading">
        <div>
          <h1>设置</h1>
          <p>配置启动方式和本机执行环境。</p>
        </div>
      </div>
      {feedback}
      <div className="settings-section">
        <h2>启动与显示</h2>
        <section className="panel">
          <label className="setting-row" htmlFor="autostart">
            <span>
              <strong>登录时显示 xrun 图标</strong>
              <small>
                {service?.development
                  ? "开发模式下不设置 App 的登录启动"
                  : "登录后自动显示菜单栏或系统托盘图标"}
              </small>
            </span>
            <span className="switch">
              <input
                id="autostart"
                type="checkbox"
                disabled={busy || !status || service?.development}
                checked={service?.app_at_login || false}
                onChange={(event) =>
                  void action({
                    command: "autostart",
                    args: { enabled: event.target.checked },
                  })
                }
              />
              <span className="switch-track" />
            </span>
          </label>
          <div className="setting-row">
            <span>
              <strong>菜单栏 / 托盘图标</strong>
              <small>隐藏后，再次打开 xrun 即可恢复图标与窗口</small>
            </span>
            <button
              className="subtle"
              disabled={busy}
              onClick={() => void action({ command: "hide_icon" })}
            >
              {pending === "hide_icon" ? "正在隐藏…" : "隐藏图标"}
            </button>
          </div>
        </section>
      </div>
      <div className="settings-section">
        <h2>执行环境</h2>
        {loadError && (
          <ErrorNotice
            title="执行环境未能读取，请重试。"
            detail={loadError}
            retry={() => void loadSettings()}
          />
        )}
        <form
          id="execution-form"
          className="panel padded"
          onSubmit={async (event) => {
            event.preventDefault();
            if (disabled || !dirty) return;
            if (
              await action({ command: "save_settings", args: { execution } })
            ) {
              // Use the saved snapshot; subsequent status reads never reset the form.
              setSettings((previous) => previous && { ...previous, execution });
              notify("执行环境已保存");
            }
          }}
        >
          <div className="field-heading">
            <label htmlFor="cwd">默认工作目录</label>
            <span className="field-tag">新任务生效</span>
          </div>
          <div className="input-with-button">
            <input
              id="cwd"
              type="text"
              placeholder={settings?.home_dir || "使用用户主目录"}
              spellCheck={false}
              disabled={disabled}
              value={cwd}
              onChange={(event) => setCwd(event.target.value)}
            />
            <button
              type="button"
              disabled={disabled}
              onClick={async () => {
                try {
                  const selected = await api.chooseDirectory();
                  if (selected) setCwd(selected);
                } catch (e) {
                  onError(e, "无法选择工作目录，请重试。");
                }
              }}
            >
              <Icon name="folder" />
              选择
            </button>
          </div>
          <p className="field-help">
            其他设备未指定工作目录时，使用此目录。留空使用用户主目录。
          </p>
          <div className="concurrency-row">
            <div>
              <label htmlFor="concurrency">同时运行的任务数</label>
              <p className="field-help">范围 1–64，仅影响新任务</p>
            </div>
            <input
              id="concurrency"
              type="number"
              min="1"
              max="64"
              required
              disabled={disabled}
              value={concurrency}
              onChange={(event) => setConcurrency(event.target.value)}
            />
          </div>
          <details className="advanced">
            <summary>工具搜索路径（PATH）</summary>
            <label className="sr-only" htmlFor="path">
              工具搜索路径
            </label>
            <textarea
              id="path"
              rows={2}
              spellCheck={false}
              placeholder="留空继承后台服务的 PATH"
              disabled={disabled}
              value={path}
              onChange={(event) => setPath(event.target.value)}
            />
            <p className="field-help">
              多个目录用{settings?.os === "windows" ? "分号（;）" : "冒号（:）"}
              分隔。留空继承后台服务的 PATH。
            </p>
          </details>
          <div className="form-footer">
            <span className={dirty ? "dirty" : ""}>
              {dirty ? "有未保存的更改" : "保存后对新任务生效，无需重启服务。"}
            </span>
            <button
              type="submit"
              className="primary"
              disabled={disabled || !dirty}
            >
              {pending === "save_settings" ? "正在保存…" : "保存更改"}
            </button>
          </div>
          {!joined && (
            <p className="field-help">加入网络后，即可配置执行环境。</p>
          )}
        </form>
      </div>
      <div className="settings-section">
        <h2>后台服务</h2>
        <section className="panel">
          <div className="setting-row">
            <span>
              <strong>
                运行状态
                <span className={`inline-status ${serviceTone}`}>{badge}</span>
              </strong>
              <small>
                {service?.development
                  ? "开发模式手动启动，退出 App 后仍继续运行"
                  : "后台服务独立于 App 运行，安装后随用户登录启动"}
              </small>
            </span>
            <button
              className="subtle"
              disabled={busy || !joined}
              onClick={() =>
                local?.daemon_running
                  ? void stop()
                  : void action({ command: "start" })
              }
            >
              {pending === "start"
                ? "正在启动…"
                : pending === "stop"
                  ? "正在停止…"
                  : local?.daemon_running
                    ? "停止服务"
                    : "启动服务"}
            </button>
          </div>
          {service?.legacy_installed && !service.development && (
            <div className="inline-notice">
              当前使用 CLI 安装的服务。停止后再次启动，将迁移为 App 的后台服务。
            </div>
          )}
          {local?.daemon_installed && (
            <div className="setting-row">
              <span>
                <strong>移除后台服务</strong>
                <small>停止任务并取消服务的登录启动，保留设备身份和记录</small>
              </span>
              <button
                className="danger-button"
                disabled={busy}
                onClick={async () => {
                  if (
                    await confirm(
                      "移除后台服务？",
                      "这会停止服务和运行中的任务，并取消服务的登录启动。设备身份和数据会保留。",
                      { label: "移除后台服务", tone: "danger" },
                    )
                  )
                    await action({ command: "remove_service" });
                }}
              >
                {pending === "remove_service" ? "正在移除…" : "移除…"}
              </button>
            </div>
          )}
        </section>
      </div>
      <div className="settings-section">
        <h2>关于</h2>
        <section className="panel">
          <div className="setting-row">
            <div className="about-brand">
              <img src="/icon.png" alt="" />
              <span>
                <strong>xrun</strong>
                <small>跨设备执行与文件传输</small>
              </span>
            </div>
            <span className="mono muted">{local?.version}</span>
          </div>
          <div className="setting-row">
            <span>
              <strong>系统</strong>
              <small>{settings ? osName(settings.os) : "正在读取…"}</small>
            </span>
          </div>
          <div className="setting-row data-row">
            <span>
              <strong>本机数据目录</strong>
              <small>身份、权限、任务记录和日志的保存位置</small>
            </span>
            <input
              type="text"
              readOnly
              aria-label="本机数据目录"
              value={settings?.data_dir || ""}
              onFocus={(event) => event.target.select()}
            />
          </div>
        </section>
      </div>
    </>
  );
}
