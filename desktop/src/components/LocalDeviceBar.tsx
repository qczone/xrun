import { useEffect, useRef } from "react";
import type { Status } from "../api";
import { useOperations } from "../app/useOperations";
import { relayState, serviceState } from "../format";
import { t } from "../i18n";
import { Icon } from "./Icon";

interface Props {
  status: Status;
  active: boolean;
  openNetworkSettings: () => void;
}
export function LocalDeviceBar({ status, active, openNetworkSettings }: Props) {
  const { busy, pending, action, stop } = useOperations();
  const { local, service } = status;
  const menu = useRef<HTMLDetailsElement>(null);
  const [serviceText, serviceTone] = serviceState(status);
  const [relayText, relayTone] = relayState(status);
  useEffect(() => {
    const dismiss = (event: MouseEvent | FocusEvent | KeyboardEvent) => {
      if (
        (event.type === "keydown" &&
          (event as KeyboardEvent).key === "Escape") ||
        (event.target instanceof Node && !menu.current?.contains(event.target))
      ) {
        if (menu.current) menu.current.open = false;
      }
    };
    document.addEventListener("click", dismiss);
    document.addEventListener("focusin", dismiss);
    document.addEventListener("keydown", dismiss);
    return () => {
      document.removeEventListener("click", dismiss);
      document.removeEventListener("focusin", dismiss);
      document.removeEventListener("keydown", dismiss);
    };
  }, []);
  useEffect(() => {
    if ((!active || busy) && menu.current) menu.current.open = false;
  }, [active, busy]);
  const paused = local.remote_access_paused;
  const label = !local.daemon_running
    ? pending === "start"
      ? t("common.starting")
      : t("service.start")
    : pending === "pause_access"
      ? t("common.updating")
      : paused
        ? t("local.resumeAccess")
        : t("local.pauseAccess");
  return (
    <div className="local-device-header">
      <header className="local-device-bar">
        <div className="local-device-avatar">
          <Icon name="monitor" />
        </div>
        <div className="local-device-identity">
          <div className="local-device-name">
            <strong>{local.name || t("common.localDevice")}</strong>
            <span>
              {t("common.localDevice")} ·{" "}
              {t(
                status.network?.is_manager ? "common.manager" : "common.member",
              )}
            </span>
          </div>
          <div className="local-device-status">
            <span>
              <i className={`dot ${serviceTone}`} />
              {t("local.serviceState", { state: serviceText })}
            </span>
            <span>
              <i className={`dot ${relayTone}`} />
              {relayText}
            </span>
            {paused && (
              <span className="access-paused">
                <i className="dot warning" />
                {t("status.paused")}
              </span>
            )}
          </div>
        </div>
        <div className="local-device-actions">
          <button
            className={!local.daemon_running ? "primary" : ""}
            disabled={busy}
            onClick={() =>
              void action(
                !local.daemon_running
                  ? { command: "start" }
                  : { command: "pause_access", args: { paused: !paused } },
              )
            }
          >
            <Icon name={!local.daemon_running || paused ? "play" : "pause"} />
            {label}
          </button>
          <details className="local-device-menu" ref={menu}>
            <summary aria-label={t("local.more")}>
              <Icon name="more" />
            </summary>
            <div className="local-device-menu-content">
              <button
                onClick={() => {
                  if (menu.current) menu.current.open = false;
                  openNetworkSettings();
                }}
              >
                {t("settings.networkService")}
              </button>
              <button
                disabled={busy || !local.daemon_running}
                onClick={() => {
                  if (menu.current) menu.current.open = false;
                  void stop();
                }}
              >
                {pending === "stop" ? t("common.stopping") : t("service.stop")}
              </button>
            </div>
          </details>
        </div>
      </header>
      {service.approval_required && (
        <div className="service-approval notice" role="status">
          <Icon name="shield" />
          <div>
            <strong>{t("local.approvalTitle")}</strong>
            <p>{t("local.approvalInstructions")}</p>
          </div>
        </div>
      )}
    </div>
  );
}
