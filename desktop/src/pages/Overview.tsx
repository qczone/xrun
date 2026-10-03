import type { Action, Status } from "../api";
import { Icon } from "../components/Icon";
import { NetworkSetup } from "../components/NetworkSetup";
import { osName, serviceLabel } from "../format";

interface Props {
  status: Status | null;
  busy: boolean;
  action: Action;
  stop: () => Promise<void>;
  navigate: (page: "devices" | "settings" | "history") => void;
}

export function Overview({ status, busy, action, stop, navigate }: Props) {
  const [badge, summary] = serviceLabel(status);
  const local = status?.local;
  const service = status?.service;
  const network = status?.network;
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>{local && !local.joined ? "连接你的设备" : "本机状态"}</h1>
          <p>
            {local && !local.joined
              ? "创建或加入网络，让你的设备互相连接。"
              : "查看连接状态，管理这台设备的后台服务。"}
          </p>
        </div>
      </div>
      {(!local || local.joined) && (
        <article className="device-hero">
          <div className="hero-top">
            <div className="device-avatar">
              <Icon name="monitor" />
            </div>
            <span
              className={`badge ${local?.daemon_connected ? "online" : ""}`}
            >
              {badge}
            </span>
          </div>
          <h2>{local?.name || "连接你的设备"}</h2>
          <p>{summary}</p>
          {local?.joined && (
            <div className="hero-actions">
              <button
                className="primary"
                disabled={busy || local.daemon_running}
                onClick={() => void action({ command: "start" })}
              >
                启动后台服务
              </button>
              <button
                disabled={busy || !local.daemon_running}
                onClick={() => void stop()}
              >
                停止服务
              </button>
              <button
                disabled={busy}
                onClick={() =>
                  void action({
                    command: "pause_access",
                    args: { paused: !local.remote_access_paused },
                  })
                }
              >
                {local.remote_access_paused ? "恢复远程访问" : "暂停远程访问"}
              </button>
            </div>
          )}
        </article>
      )}
      {service?.approval_required && (
        <div className="notice">
          <Icon name="shield" />
          <div>
            <strong>需要系统授权</strong>
            <p>在系统设置 → 通用 → 登录项中允许 xrun，然后启动服务。</p>
          </div>
        </div>
      )}
      {local?.joined && (
        <section className="panel">
          <div className="panel-row">
            <span>本机角色</span>
            <strong>
              {network
                ? network.is_manager
                  ? "管理设备"
                  : "普通设备"
                : "网络信息不可用"}
            </strong>
          </div>
          {network && (
            <>
              <div className="panel-row">
                <span>管理设备</span>
                <span>{network.manager_name}</span>
              </div>
              <div className="panel-row">
                <span>中转地址</span>
                <div className="relay-addresses">
                  {network.relay_addresses.map((address) => (
                    <code key={address}>{address}</code>
                  ))}
                </div>
              </div>
            </>
          )}
          <div className="panel-row">
            <span>设备名称</span>
            <strong>{local.name}</strong>
          </div>
          <div className="panel-row">
            <span>设备 ID</span>
            <code>{local.device_id}</code>
          </div>
          <div className="panel-row">
            <span>系统</span>
            <span>
              {osName(
                navigator.platform.startsWith("Mac")
                  ? "macos"
                  : navigator.platform.startsWith("Win")
                    ? "windows"
                    : "linux",
              )}
            </span>
          </div>
          <div className="panel-row">
            <span>安装方式</span>
            <span>
              {service?.legacy_installed
                ? "CLI 安装的服务"
                : service?.installed
                  ? "App 后台服务"
                  : "尚未安装服务"}
            </span>
          </div>
        </section>
      )}
      {local && (
        <NetworkSetup
          joined={local.joined}
          busy={busy}
          action={action}
          onCreated={() => navigate("devices")}
        />
      )}
      <div className="quick-links">
        <button onClick={() => navigate("devices")}>
          <Icon name="shield" />
          <span>
            <strong>管理设备权限</strong>
            <small>选择可以访问本机的设备</small>
          </span>
          <Icon name="arrow" className="arrow" />
        </button>
        <button onClick={() => navigate("settings")}>
          <Icon name="settings" />
          <span>
            <strong>配置执行环境</strong>
            <small>工作目录、工具路径与并发</small>
          </span>
          <Icon name="arrow" className="arrow" />
        </button>
      </div>
      <button className="history-shortcut" onClick={() => navigate("history")}>
        <Icon name="terminal" />
        查看本机任务与日志
        <Icon name="arrow" className="arrow" />
      </button>
      <p className="footnote">关闭窗口或退出 App，后台服务会继续运行。</p>
    </>
  );
}
