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
  transferFile: (device: Device) => void;
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
  transferFile,
}: Props) {
  const { busy, confirm, action, notify } = useOperations();
  const network = status?.network;
  const manager = !!network?.is_manager;
  const denied = !!status?.deny_from.includes(device.device_id);
  const individual = !!status?.allow_from.includes(device.device_id);
  const allowed =
    !device.revoked &&
    !denied &&
    (individual || (!manager && !!status?.local.allow_all));
  const online = device.online && !device.revoked;
  const source = device.revoked
    ? t("devices.membershipRevoked")
    : denied
      ? t("devices.individualDeny")
      : individual
        ? t("devices.individualAllow")
        : !manager && status?.local.allow_all
          ? t("devices.fromAll")
          : t("devices.notAllowed");
  const actions = [
    {
      icon: "terminal",
      label: t("remote.open"),
      named: t("remote.openNamed", { name: device.name }),
      open: runCommand,
    },
    {
      icon: "transfer",
      label: t("files.open"),
      named: t("files.openNamed", { name: device.name }),
      open: transferFile,
    },
    {
      icon: "camera",
      label: t("capture.open"),
      named: t("capture.openNamed", { name: device.name }),
      open: captureScreen,
    },
  ] as const;
  return (
    <div className={`device-row ${online ? "online" : ""}`}>
      <div className="device-identity">
        <div className="device-icon">
          <Icon name="monitor" />
          <i className={`dot ${online ? "online" : ""}`} />
        </div>
        <div className="device-details">
          <div className="device-name">
            {device.name}
            {device.admin && (
              <span className="role-label">{t("common.manager")}</span>
            )}
          </div>
          <div className="device-info">
            <span className="device-presence">
              {device.revoked
                ? t("devices.revoked")
                : device.online
                  ? t("devices.online")
                  : t("devices.offline")}
            </span>
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
      <div className="device-actions">
        {online &&
          actions.map((item) => (
            <button
              key={item.icon}
              className="icon-button"
              aria-label={item.named}
              title={item.label}
              disabled={busy}
              onClick={() => item.open(device)}
            >
              <Icon name={item.icon} />
            </button>
          ))}
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
                  t(
                    manager
                      ? "devices.allowManagerMessage"
                      : "devices.allowMessage",
                  ),
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
