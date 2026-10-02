import type { Action, Confirm, Device, Status } from "../api";
import { Icon } from "../components/Icon";
import { osName } from "../format";

interface Props {
  status: Status | null;
  devices: Device[];
  busy: boolean;
  loading: boolean;
  message: string;
  refresh: () => Promise<void>;
  action: Action;
  confirm: Confirm;
  notify: (message: string) => void;
}

export function Devices({
  status,
  devices,
  busy,
  loading,
  message,
  refresh,
  action,
  confirm,
  notify,
}: Props) {
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>设备</h1>
          <p>管理其他设备对本机的访问权限。</p>
        </div>
        <button
          disabled={busy || loading || !status?.local.joined}
          onClick={() => void refresh()}
        >
          <Icon name="refresh" />
          刷新
        </button>
      </div>
      <div className="permission-note">
        <Icon name="shield" />
        <p>
          允许的设备可以用你的用户权限执行命令、传输文件和截图。只授权你信任的设备。
        </p>
      </div>
      <section className="panel">
        <div className="list-heading">
          <span>设备</span>
          <span>允许访问本机</span>
        </div>
        <div id="devices">
          {devices.map((device) => (
            <div className="device" key={device.device_id}>
              <div className="device-icon">
                <Icon name="monitor" />
              </div>
              <div className="device-details">
                <div className="device-name">{device.name}</div>
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
              <label className="switch">
                <input
                  type="checkbox"
                  disabled={busy || device.revoked}
                  aria-label={`允许 ${device.name} 访问本机`}
                  checked={
                    !device.revoked &&
                    !!status?.allow_from.includes(device.device_id)
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
                      notify(allow ? "已允许访问本机" : "已取消访问权限");
                  }}
                />
                <span className="switch-track" />
              </label>
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
