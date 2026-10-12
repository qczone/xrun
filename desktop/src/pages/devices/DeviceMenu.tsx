import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { Device } from "../../api";
import { useOperations } from "../../app/useOperations";
import { Icon } from "../../components/Icon";
import { Identifier } from "../../components/Identifier";
import { archName, dateTime, osName } from "../../format";
import { t } from "../../i18n";

interface Props {
  device: Device;
  manager: boolean;
  managerId?: string;
  open: boolean;
  toggle: () => void;
  close: () => void;
  revoke: () => void;
  runCommand: () => void;
  captureScreen: () => void;
  transferFile: () => void;
}
export function DeviceMenu({
  device,
  manager,
  managerId,
  open,
  toggle,
  close,
  revoke,
  runCommand,
  captureScreen,
  transferFile,
}: Props) {
  const { busy } = useOperations();
  const menu = useRef<HTMLDetailsElement>(null);
  const trigger = useRef<HTMLElement>(null);
  const body = useRef<HTMLDivElement>(null);
  const [placement, setPlacement] = useState("below");
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (event.target instanceof Node && !menu.current?.contains(event.target))
        close();
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open, close]);
  useLayoutEffect(() => {
    if (!open || !body.current || !trigger.current) return;
    const bounds = body.current.getBoundingClientRect();
    const anchor =
      menu.current?.getBoundingClientRect() ||
      trigger.current.getBoundingClientRect();
    const container = menu.current?.closest("main")?.getBoundingClientRect();
    const bottom = Math.min(
      window.innerHeight,
      container?.bottom ?? window.innerHeight,
    );
    const top = Math.max(0, container?.top ?? 0);
    setPlacement(
      anchor.bottom + bounds.height + 6 > bottom - 8 &&
        anchor.top - bounds.height - 6 >= top + 8
        ? "above"
        : "below",
    );
  }, [
    open,
    device.os,
    device.arch,
    device.daemon_version,
    device.last_seen,
    device.online,
  ]);
  return (
    <details
      ref={menu}
      className="device-more"
      open={open}
      data-placement={placement}
      onBlur={(event) => {
        if (
          !(event.relatedTarget instanceof Node) ||
          !event.currentTarget.contains(event.relatedTarget)
        )
          close();
      }}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          close();
          trigger.current?.focus();
        }
      }}
    >
      <summary
        ref={trigger}
        aria-label={t("devices.moreNamed", { name: device.name })}
        aria-expanded={open}
        onClick={(event) => {
          event.preventDefault();
          toggle();
        }}
      >
        <Icon name="more" />
      </summary>
      <div ref={body} className="device-more-body">
        <span className="field-help">{t("common.deviceId")}</span>
        <Identifier value={device.device_id} label={t("common.deviceId")} />
        <dl className="device-facts">
          {device.connection && (
            <>
              <dt>{t("devices.connection")}</dt>
              <dd>
                {t(
                  device.connection === "direct"
                    ? "devices.direct"
                    : "devices.relay",
                )}
              </dd>
            </>
          )}
          <dt>{t("devices.system")}</dt>
          <dd>{device.os ? osName(device.os) : t("devices.infoPending")}</dd>
          {device.arch && (
            <>
              <dt>{t("devices.arch")}</dt>
              <dd>{archName(device.arch)}</dd>
            </>
          )}
          {device.daemon_version && (
            <>
              <dt>{t("devices.version")}</dt>
              <dd>{device.daemon_version}</dd>
            </>
          )}
          {!device.online && device.last_seen != null && (
            <>
              <dt>{t("devices.lastSeen")}</dt>
              <dd>{dateTime(device.last_seen)}</dd>
            </>
          )}
        </dl>
        <button
          className="remote-command-entry"
          disabled={busy || device.revoked || !device.online}
          onClick={() => {
            close();
            runCommand();
          }}
        >
          <Icon name="terminal" />
          {t("remote.open")}
        </button>
        <button
          disabled={busy || device.revoked || !device.online || !!device.admin}
          onClick={() => {
            close();
            captureScreen();
          }}
        >
          <Icon name="monitor" />
          {t("capture.open")}
        </button>
        <button
          disabled={busy || device.revoked || !device.online || !!device.admin}
          onClick={() => {
            close();
            transferFile();
          }}
        >
          {t("files.open")}
        </button>
        {manager && !device.revoked && device.device_id !== managerId && (
          <button
            className="danger-button"
            disabled={busy}
            aria-label={t("revoke.named", { name: device.name })}
            onClick={revoke}
          >
            {t("revoke.menu")}
          </button>
        )}
      </div>
    </details>
  );
}
