import type { Device, Status } from "../../api";
import { useOperations } from "../../app/useOperations";
import { Icon } from "../../components/Icon";
import { archName, compactIdentifier, osName } from "../../format";
import { t } from "../../i18n";
import { DeviceMenu } from "./DeviceMenu";
interface Props {
  device: Device;
  status: Status | null;
  open: boolean;
  toggle: () => void;
  close: () => void;
  revoke: (device: Device) => Promise<void>;
  runCommand: (device: Device) => void;
  captureScreen: (device: Device) => void;
}
export function DeviceRow({
  device,
  status,
  open,
  toggle,
  close,
  revoke,
  runCommand,
  captureScreen,
}: Props) {
  const { busy, confirm, action, notify } = useOperations();
  const network = status?.network;
  const manager = !!network?.is_manager;
  const denied = !!status?.deny_from.includes(device.device_id);
  const individual = !!status?.allow_from.includes(device.device_id);
  const allowed =
    !manager &&
    !device.revoked &&
    !denied &&
    (!!status?.local.allow_all || individual);
  const source = manager
    ? t("devices.managerProtected")
    : device.revoked
      ? t("devices.membershipRevoked")
      : denied
        ? t("devices.individualDeny")
        : individual
          ? t("devices.individualAllow")
          : status?.local.allow_all
            ? t("devices.fromAll")
            : t("devices.notAllowed");
  return (
    <div className="device-row" key={device.device_id}>
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
            <span>
              {device.os
                ? [osName(device.os), archName(device.arch)]
                    .filter(Boolean)
                    .join(" · ")
                : t("devices.infoPending")}
            </span>
            <code>{compactIdentifier(device.device_id)}</code>
          </div>
        </div>
      </div>
      <div className="device-connection">
        <span
          className={`dot ${device.online && !device.revoked ? "online" : ""}`}
        />
        <span>
          {device.revoked
            ? t("devices.revoked")
            : device.online
              ? t("devices.online")
              : t("devices.offline")}
        </span>
      </div>
      <div className="device-permission">
        <label className="switch">
          <input
            type="checkbox"
            disabled={
              busy || manager || device.revoked || !status?.local.joined
            }
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
        <small title={source}>
          {allowed ? t("devices.allowedShort") : t("devices.deniedShort")}
        </small>
      </div>
      <DeviceMenu
        runCommand={() => runCommand(device)}
        captureScreen={() => captureScreen(device)}
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
