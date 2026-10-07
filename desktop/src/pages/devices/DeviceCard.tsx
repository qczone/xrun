import type { Device, Status } from "../../api";
import { useOperations } from "../../app/useOperations";
import { Icon } from "../../components/Icon";
import { osName } from "../../format";
import { t } from "../../i18n";
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
    ? t("devices.membershipRevoked")
    : denied
      ? t("devices.individualDeny")
      : individual
        ? t("devices.individualAllow")
        : status?.local.allow_all
          ? t("devices.fromAll")
          : t("devices.notAllowed");
  return (
    <div className="device" key={device.device_id}>
      <div className="device-identity">
        <div className="device-icon">
          <Icon name="monitor" />
        </div>
        <div className="device-details">
          <div className="device-name">
            {device.name}
            {device.admin && (
              <span className="role-label">{t("common.manager")}</span>
            )}
          </div>
          <div className="device-info">
            <span
              className={`dot ${device.online && !device.revoked ? "online" : ""}`}
            />
            <span>
              {device.revoked
                ? t("devices.revoked")
                : device.online
                  ? t("devices.connected")
                  : t("devices.disconnected")}
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
            aria-label={t("devices.allowNamed", { name: device.name })}
            checked={allowed}
            onChange={async (event) => {
              const allow = event.target.checked;
              if (
                allow &&
                !(await confirm(
                  t("devices.allowTitle", { name: device.name }),
                  t("devices.allowMessage"),
                  { label: t("devices.allowConfirm") },
                ))
              )
                return;
              if (
                await action({
                  command: "permission",
                  args: { device: device.device_id, allow },
                })
              )
                notify(allow ? "devices.allowed" : "devices.denied");
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
