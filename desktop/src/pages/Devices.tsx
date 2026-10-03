import { useEffect, useState } from "react";
import type {
  Action,
  Confirm,
  Device,
  Operation,
  Revocation,
  Status,
} from "../api";
import { Icon } from "../components/Icon";
import { InvitePanel } from "../components/InvitePanel";
import { osName } from "../format";

interface Props {
  active: boolean;
  status: Status | null;
  devices: Device[];
  busy: boolean;
  loading: boolean;
  message: string;
  refresh: () => Promise<void>;
  action: Action;
  confirm: Confirm;
  notify: (message: string) => void;
  operate: Operation;
  revoke: (device: string) => Promise<Revocation | undefined>;
}

export function Devices({
  active,
  status,
  devices,
  busy,
  loading,
  message,
  refresh,
  action,
  confirm,
  notify,
  operate,
  revoke,
}: Props) {
  const [revocation, setRevocation] = useState<Revocation | null>(null);
  const network = status?.network;
  useEffect(() => setRevocation(null), [network?.network_id]);
  const deviceName = (id: string) =>
    devices.find((d) => d.device_id === id)?.name || id;
  const revokeMember = async (device: Device) => {
    if (
      !(await confirm(
        `撤销 ${device.name}？`,
        "收到撤销记录的设备将拒绝该成员。离线设备需要等收到更新后才生效；已经受理的后台任务不会自动取消。重新加入需要新的身份和邀请。",
      ))
    )
      return;
    const result = await revoke(device.device_id);
    if (result) setRevocation(result);
  };
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>设备</h1>
          <p>
            {status?.network?.is_manager
              ? "管理网络成员，以及其他设备对本机的访问权限。"
              : "管理其他设备对本机的访问权限；邀请和撤销由管理设备负责。"}
          </p>
        </div>
        <button
          disabled={busy || loading || !status?.local.joined}
          onClick={() => void refresh()}
        >
          <Icon name="refresh" />
          刷新
        </button>
      </div>
      <InvitePanel
        active={active}
        status={status}
        busy={busy}
        operate={operate}
        confirm={confirm}
        notify={notify}
      />
      {revocation && (
        <section className="panel padded revocation-result" role="status">
          <div className="page-heading">
            <h2>已撤销 {deviceName(revocation.device_id)}</h2>
            <button
              onClick={() => setRevocation(null)}
              aria-label="关闭撤销结果"
            >
              关闭
            </button>
          </div>
          <p>撤销记录已在本机保存。</p>
          {revocation.relay_error && (
            <p className="warning-text">
              尚未发布到中转：{revocation.relay_error}
            </p>
          )}
          {revocation.undelivered.length > 0 ? (
            <>
              <p>以下设备尚未确认收到这次更新：</p>
              <ul>
                {revocation.undelivered.map((id) => (
                  <li key={id}>
                    {deviceName(id)} <code>{id}</code>
                  </li>
                ))}
              </ul>
              <p>
                这些设备收到更新前，可能仍接受被撤销成员。紧急阻断可以在对应设备上暂停远程访问。
              </p>
            </>
          ) : !revocation.relay_error ? (
            <p>当前其他成员均已确认收到更新。</p>
          ) : null}
          {(revocation.relay_error || revocation.undelivered.length > 0) &&
            network?.is_manager && (
              <button
                disabled={busy}
                onClick={async () => {
                  const result = await revoke(revocation.device_id);
                  if (result) setRevocation(result);
                }}
              >
                重新发布撤销记录
              </button>
            )}
        </section>
      )}
      {status?.local.remote_access_paused && (
        <div className="notice">
          <Icon name="shield" />
          <p>远程访问已暂停，以下授权暂不生效；恢复后沿用这些设置。</p>
        </div>
      )}
      <section className="panel padded">
        <div className="panel-row">
          <div>
            <strong>允许所有设备</strong>
            <p className="field-help">
              包括以后通过邀请加入的设备；单独拒绝的设备除外。关闭后保留单独授权。
            </p>
          </div>
          <label className="switch">
            <input
              type="checkbox"
              aria-label="允许所有设备访问本机"
              disabled={busy || !status?.local.joined}
              checked={!!status?.local.allow_all}
              onChange={async (event) => {
                const allow = event.target.checked;
                if (
                  allow &&
                  !(await confirm(
                    "允许所有设备访问本机？",
                    "当前和以后加入的设备都可以用你的用户权限执行命令、传文件和截图。获得邀请的人加入后也会获得访问权；单独拒绝仍然生效。",
                  ))
                )
                  return;
                if (
                  await action({ command: "all_permissions", args: { allow } })
                )
                  notify(
                    allow
                      ? "已允许当前和未来设备"
                      : "已关闭全体授权，保留单独权限",
                  );
              }}
            />
            <span className="switch-track" />
          </label>
        </div>
      </section>
      <div className="permission-note">
        <Icon name="shield" />
        <p>
          允许的设备可以用你的用户权限执行命令、传输文件和截图。只授权你信任的设备。
        </p>
      </div>
      <section className="panel">
        <div className="list-heading">
          <span>设备</span>
          <span>
            {network?.is_manager ? "本机访问与成员管理" : "允许访问本机"}
          </span>
        </div>
        <div id="devices">
          {devices.map((device) => (
            <div className="device" key={device.device_id}>
              <div className="device-icon">
                <Icon name="monitor" />
              </div>
              <div className="device-details">
                <div className="device-name">
                  {device.name}
                  {device.admin && <span className="role-label">管理设备</span>}
                </div>
                <div className="device-info">
                  <span
                    className={`dot ${device.online && !device.revoked ? "online" : ""}`}
                  />
                  <span>
                    {device.revoked
                      ? "已撤销"
                      : device.online
                        ? "在线"
                        : "离线"}{" "}
                    · {osName(device.os)}
                  </span>
                </div>
                <div className="device-id mono">{device.device_id}</div>
              </div>
              <div className="device-controls">
                <label className="switch">
                  <input
                    type="checkbox"
                    disabled={busy || device.revoked}
                    aria-label={`允许 ${device.name} 访问本机`}
                    checked={
                      !device.revoked &&
                      !status?.deny_from.includes(device.device_id) &&
                      (!!status?.local.allow_all ||
                        !!status?.allow_from.includes(device.device_id))
                    }
                    onChange={async (event) => {
                      const allow = event.target.checked;
                      if (
                        allow &&
                        !(await confirm(
                          `允许 ${device.name} 访问本机？`,
                          "该设备将能以你的用户权限执行命令、传输文件和截图。请只授权你信任的设备。",
                        ))
                      )
                        return;
                      if (
                        await action({
                          command: "permission",
                          args: { device: device.device_id, allow },
                        })
                      )
                        notify(allow ? "已允许访问本机" : "已拒绝访问本机");
                    }}
                  />
                  <span className="switch-track" />
                </label>
                {network?.is_manager &&
                  !device.revoked &&
                  device.device_id !== network.manager_id && (
                    <button
                      className="danger-button"
                      disabled={busy}
                      aria-label={`撤销 ${device.name}`}
                      onClick={() => void revokeMember(device)}
                    >
                      撤销
                    </button>
                  )}
              </div>
            </div>
          ))}
        </div>
        {(!devices.length || loading || message.startsWith("无法查询")) && (
          <div className="empty-state">{message}</div>
        )}
      </section>
      <p className="footnote">
        这里的授权只影响本机。要访问其他设备，还需要对方允许本机。
      </p>
    </>
  );
}
