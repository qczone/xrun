import type { ReactNode } from "react";
import type { Status } from "../api";
import { useOperations } from "../app/useOperations";
import { Icon } from "../components/Icon";
import { NetworkSetup } from "../components/NetworkSetup";
import { relayHost, relayState, serviceState } from "../format";

interface Props {
  status: Status | null;
  feedback: ReactNode;
  navigate: (page: "devices" | "settings" | "history") => void;
}

export function Overview({ status, feedback, navigate }: Props) {
  const { busy, pending, action, stop } = useOperations();
  const local = status?.local;
  const service = status?.service;
  const network = status?.network;
  const [serviceText, serviceTone] = serviceState(status);
  const [relayText, relayTone] = relayState(status);
  const allowed =
    status?.allow_from.filter((id) => !status.deny_from.includes(id)).length ||
    0;
  const paused = !!local?.remote_access_paused;
  const accessText = !local
    ? "检查中"
    : paused
      ? "已暂停"
      : local.allow_all
        ? "全体成员可访问"
        : "按设备授权";

  return (
    <>
      <div className="page-heading">
        <div>
          <h1>{local && !local.joined ? "连接你的设备" : "本机概览"}</h1>
          <p>
            {local && !local.joined
              ? "加入网络，让你的设备通过 xrun 互相连接。"
              : "查看本机连接与访问状态，管理后台服务。"}
          </p>
        </div>
      </div>
      {feedback}
      {(!local || local.joined) && (
        <section className="panel overview-status" aria-label="本机运行状态">
          <div className="overview-device">
            <div className="device-avatar">
              <Icon name="monitor" />
            </div>
            <div className="overview-identity">
              <h2>{local?.name || "正在读取本机…"}</h2>
              <span className="muted">
                {network
                  ? network.is_manager
                    ? "管理设备"
                    : "普通设备"
                  : "本机设备"}
              </span>
            </div>
            {local?.joined && (
              <div className="overview-actions">
                {!local.daemon_running ? (
                  <button
                    className="primary"
                    disabled={busy}
                    onClick={() => void action({ command: "start" })}
                  >
                    {pending === "start" ? "正在启动…" : "启动后台服务"}
                  </button>
                ) : paused ? (
                  <button
                    className="primary"
                    disabled={busy}
                    onClick={() =>
                      void action({
                        command: "pause_access",
                        args: { paused: false },
                      })
                    }
                  >
                    {pending === "pause_access" ? "正在恢复…" : "恢复远程访问"}
                  </button>
                ) : (
                  <>
                    <button
                      className="primary"
                      onClick={() => navigate("devices")}
                    >
                      谁能访问本机
                    </button>
                    <button
                      disabled={busy}
                      onClick={() =>
                        void action({
                          command: "pause_access",
                          args: { paused: true },
                        })
                      }
                    >
                      {pending === "pause_access"
                        ? "正在暂停…"
                        : "暂停远程访问"}
                    </button>
                  </>
                )}
              </div>
            )}
          </div>
          <div className="status-grid">
            <div>
              <span className="status-label">后台服务</span>
              <strong className={`status-value ${serviceTone}`}>
                <span className={`dot ${serviceTone}`} />
                {serviceText}
              </strong>
              <small>
                {service?.approval_required
                  ? "请在系统设置中允许后台运行"
                  : local?.daemon_running
                    ? "独立于 App 运行"
                    : "启动后才可接受远程操作"}
              </small>
            </div>
            <div>
              <span className="status-label">中转连接</span>
              <strong className={`status-value ${relayTone}`}>
                <span className={`dot ${relayTone}`} />
                {relayText}
              </strong>
              <small>
                {local?.daemon_connected === null
                  ? "更新后台服务后可查看"
                  : local?.daemon_running && local.daemon_connected === false
                    ? "网络恢复后自动重连"
                    : "用于设备发现与连接"}
              </small>
            </div>
            <div>
              <span className="status-label">远程访问</span>
              <strong className={`status-value ${paused ? "warning" : ""}`}>
                <Icon name="shield" />
                {accessText}
              </strong>
              <small>
                {paused
                  ? "恢复后沿用已有授权"
                  : local?.allow_all
                    ? "单独拒绝的设备除外"
                    : `已单独授权 ${allowed} 台设备`}
              </small>
            </div>
          </div>
        </section>
      )}
      {service?.approval_required && (
        <div className="notice">
          <Icon name="shield" />
          <div>
            <strong>需要系统授权</strong>
            <p>在系统设置 → 通用 → 登录项中允许 xrun，然后启动后台服务。</p>
          </div>
        </div>
      )}
      {local?.joined && paused && (
        <div className="notice">
          <Icon name="shield" />
          <div>
            <strong>远程访问已暂停</strong>
            <p>
              已受理的可靠任务继续运行；流式执行和转发连接关闭。恢复访问后沿用已有授权。
            </p>
            {!local.daemon_running && (
              <button
                className="text-button"
                disabled={busy}
                onClick={() =>
                  void action({
                    command: "pause_access",
                    args: { paused: false },
                  })
                }
              >
                {pending === "pause_access" ? "正在恢复…" : "恢复远程访问"}
              </button>
            )}
          </div>
        </div>
      )}
      {local?.joined && (
        <section className="panel network-summary">
          <div className="panel-heading">
            <h2>网络信息</h2>
            <span className="muted">
              {network ? "端到端加密" : "暂时不可用"}
            </span>
          </div>
          {network && (
            <div className="network-facts">
              <div>
                <span>管理设备</span>
                <strong>{network.manager_name}</strong>
              </div>
              <div>
                <span>中转</span>
                <strong>
                  {network.relay_addresses.map(relayHost).join(" · ")}
                </strong>
              </div>
            </div>
          )}
          <details className="overview-details">
            <summary>设备与网络详情</summary>
            <div className="panel-row">
              <span>设备 ID</span>
              <code>{local.device_id}</code>
            </div>
            {network && (
              <div className="panel-row">
                <span>完整中转地址</span>
                <div className="relay-addresses">
                  {network.relay_addresses.map((address) => (
                    <code key={address}>{address}</code>
                  ))}
                </div>
              </div>
            )}
            <div className="panel-row">
              <span>服务安装</span>
              <span>
                {service?.development
                  ? "开发模式"
                  : service?.legacy_installed
                    ? "CLI 安装的服务"
                    : service?.installed
                      ? "App 后台服务"
                      : "尚未安装服务"}
              </span>
            </div>
            <div className="details-actions">
              <button
                className="text-button"
                onClick={() => navigate("settings")}
              >
                后台服务设置
              </button>
              {local.daemon_running && (
                <button
                  className="danger-button"
                  disabled={busy}
                  onClick={() => void stop()}
                >
                  {pending === "stop" ? "正在停止…" : "停止后台服务…"}
                </button>
              )}
            </div>
          </details>
        </section>
      )}
      {local && (
        <NetworkSetup
          joined={local.joined}
          busy={busy}
          pending={pending}
          startFailed={status?.error?.code === "SERVICE_START_FAILED"}
          action={action}
          onCreated={() => navigate("devices")}
        />
      )}
      {local?.joined && (
        <>
          <div className="quick-links">
            <button onClick={() => navigate("devices")}>
              <Icon name="shield" />
              <span>
                <strong>谁能访问本机</strong>
                <small>查看设备，管理访问授权</small>
              </span>
              <Icon name="arrow" className="arrow" />
            </button>
            <button onClick={() => navigate("settings")}>
              <Icon name="settings" />
              <span>
                <strong>执行环境</strong>
                <small>工作目录、工具路径与任务数量</small>
              </span>
              <Icon name="arrow" className="arrow" />
            </button>
          </div>
          <button
            className="history-shortcut"
            onClick={() => navigate("history")}
          >
            <Icon name="terminal" />
            查看本机活动记录
            <Icon name="arrow" className="arrow" />
          </button>
          <p className="footnote">关闭窗口或退出 App，后台服务会继续运行。</p>
        </>
      )}
    </>
  );
}
