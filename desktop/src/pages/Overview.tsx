import { useEffect, useState } from "react";
import type { Action, Status } from "../api";
import { Icon } from "../components/Icon";
import { osName, serviceLabel } from "../format";

interface Props {
  status: Status | null;
  busy: boolean;
  action: Action;
  stop: () => Promise<void>;
  navigate: (page: "devices" | "settings" | "history") => void;
}

export function Overview({ status, busy, action, stop, navigate }: Props) {
  const [link, setLink] = useState("");
  const [name, setName] = useState("");
  const [badge, summary] = serviceLabel(status);
  const local = status?.local;
  const service = status?.service;
  const network = status?.network;
  useEffect(() => {
    if (local?.joined) setLink("");
  }, [local?.joined]);
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>本机状态</h1>
          <p>查看连接状态，管理这台设备的后台服务。</p>
        </div>
      </div>
      <article className="device-hero">
        <div className="hero-top">
          <div className="device-avatar">
            <Icon name="monitor" />
          </div>
          <span className={`badge ${local?.daemon_connected ? "online" : ""}`}>
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
      {local && !local.joined && (
        <section className="panel padded">
          <div className="section-title">
            <h2>加入已有部署</h2>
            <p>
              在管理设备上运行 <code>xrun invite</code>，然后粘贴邀请链接。
            </p>
          </div>
          <form
            id="join-form"
            onSubmit={async (event) => {
              event.preventDefault();
              if (await action({ command: "join", args: { link, name } }))
                setLink("");
            }}
          >
            <label htmlFor="link">邀请链接</label>
            <input
              id="link"
              type="password"
              placeholder="xrun://…"
              autoComplete="off"
              spellCheck={false}
              required
              disabled={busy}
              value={link}
              onChange={(event) => setLink(event.target.value)}
            />
            <label htmlFor="name">本机名称</label>
            <input
              id="name"
              type="text"
              placeholder="mac1 或 win1"
              pattern="[a-z][a-z0-9-]{0,31}"
              maxLength={32}
              autoComplete="off"
              spellCheck={false}
              required
              disabled={busy}
              value={name}
              onChange={(event) => setName(event.target.value)}
            />
            <p className="field-help">小写字母、数字和短横线，以字母开头。</p>
            <button className="primary" type="submit" disabled={busy}>
              加入并启动服务
              <Icon name="arrow" />
            </button>
          </form>
        </section>
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
