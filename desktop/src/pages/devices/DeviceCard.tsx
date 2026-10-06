import type { Device, Status } from "../../api";
import { useOperations } from "../../app/useOperations";
import { Icon } from "../../components/Icon";
import { osName } from "../../format";
import { DeviceMenu } from "./DeviceMenu";
interface Props {
  device: Device;
  status: Status | null;
  open: boolean;
  toggle: () => void;
  close: () => void;
  revoke: (device: Device) => Promise<void>;
}
export function DeviceCard({
  device,
  status,
  open,
  toggle,
  close,
  revoke,
}: Props) {
  const { busy, confirm, action, notify } = useOperations();
  const network = status?.network;
  const manager = !!network?.is_manager;
  const denied = !!status?.deny_from.includes(device.device_id);
  const individual = !!status?.allow_from.includes(device.device_id);
  const allowed =
    !device.revoked && !denied && (!!status?.local.allow_all || individual);
  const source = device.revoked
    ? "成员已撤销"
    : denied
      ? "单独拒绝"
      : individual
        ? "单独授权"
        : status?.local.allow_all
          ? "来自全体授权"
          : "未授权";
  return (
    <div className="device" key={device.device_id}>
      <div className="device-identity">
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
                  ? "中转报告已连接"
                  : "未连接中转"}
              {device.os && <> · {osName(device.os)}</>}
            </span>
          </div>
        </div>
      </div>
      <div className="device-permission">
        <label className="switch">
          <input
            type="checkbox"
            disabled={busy || device.revoked || !status?.local.joined}
            aria-label={`允许 ${device.name} 访问本机`}
            checked={allowed}
            onChange={async (event) => {
              const allow = event.target.checked;
              if (
                allow &&
                !(await confirm(
                  `允许 ${device.name} 访问本机？`,
                  "该设备将能以你的用户权限执行命令、传输文件和截图。请只授权你信任的设备。",
                  { label: "允许访问本机" },
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
        <small>{source}</small>
      </div>
      <DeviceMenu
        device={device}
        manager={manager}
        managerId={network?.manager_id}
        open={open}
        toggle={toggle}
        close={close}
        revoke={() => {
          close();
          void revoke(device);
        }}
      />
    </div>
  );
}
