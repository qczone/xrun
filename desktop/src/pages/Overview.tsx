import type { ReactNode } from "react";
import type { Device, Status } from "../api";
import { TrafficPanel } from "../components/TrafficPanel";
import { useOperations } from "../app/useOperations";
import { Icon } from "../components/Icon";
import { NetworkSetup } from "../components/NetworkSetup";
import { relayHost, relayState, serviceState } from "../format";
import { t } from "../i18n";

interface Props {
  active: boolean;
  devices: Device[];
  status: Status | null;
  feedback: ReactNode;
  navigate: (page: "devices" | "settings" | "history") => void;
}

export function Overview({
  status,
  feedback,
  navigate,
  active,
  devices,
}: Props) {
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
    ? t("common.checking")
    : paused
      ? t("overview.paused")
      : local.allow_all
        ? t("overview.allMembers")
        : t("overview.perDevice");

  return (
    <>
      <div className="page-heading">
        <div>
          <h1>
            {local && !local.joined
              ? t("overview.connectTitle")
              : t("overview.title")}
          </h1>
          <p>
            {local && !local.joined
              ? t("overview.connectDescription")
              : t("overview.description")}
          </p>
        </div>
      </div>
      {feedback}
      {(!local || local.joined) && (
        <section
          className="panel overview-status"
          aria-label={t("overview.deviceStatus")}
        >
          <div className="overview-device">
            <div className="device-avatar">
              <Icon name="monitor" />
            </div>
            <div className="overview-identity">
              <h2>{local?.name || t("overview.reading")}</h2>
              <span className="muted">
                {network
                  ? network.is_manager
                    ? t("common.manager")
                    : t("common.member")
                  : t("common.localDevice")}
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
                    {pending === "start"
                      ? t("common.starting")
                      : t("service.startBackground")}
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
                    {pending === "pause_access"
                      ? t("common.resuming")
                      : t("overview.resume")}
                  </button>
                ) : (
                  <>
                    <button
                      className="primary"
                      onClick={() => navigate("devices")}
                    >
                      {t("overview.whoHasAccess")}
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
                        ? t("common.pausing")
                        : t("overview.pause")}
                    </button>
                  </>
                )}
              </div>
            )}
          </div>
          <div className="status-grid">
            <div>
              <span className="status-label">{t("service.title")}</span>
              <strong className={`status-value ${serviceTone}`}>
                <span className={`dot ${serviceTone}`} />
                {serviceText}
              </strong>
              <small>
                {service?.approval_required
                  ? t("overview.approvalHint")
                  : local?.daemon_running
                    ? t("overview.independent")
                    : t("overview.startHint")}
              </small>
            </div>
            <div>
              <span className="status-label">
                {t("overview.relayConnection")}
              </span>
              <strong className={`status-value ${relayTone}`}>
                <span className={`dot ${relayTone}`} />
                {relayText}
              </strong>
              <small>
                {local?.daemon_connected === null
                  ? t("overview.updateHint")
                  : local?.daemon_running && local.daemon_connected === false
                    ? t("overview.reconnectHint")
                    : t("overview.relayHint")}
              </small>
            </div>
            <div>
              <span className="status-label">{t("overview.remoteAccess")}</span>
              <strong className={`status-value ${paused ? "warning" : ""}`}>
                <Icon name="shield" />
                {accessText}
              </strong>
              <small>
                {paused
                  ? t("overview.resumeHint")
                  : local?.allow_all
                    ? t("overview.denyException")
                    : t("overview.allowedCount", { count: allowed })}
              </small>
            </div>
          </div>
        </section>
      )}
      {service?.approval_required && (
        <div className="notice">
          <Icon name="shield" />
          <div>
            <strong>{t("overview.approvalTitle")}</strong>
            <p>{t("overview.approvalInstructions")}</p>
          </div>
        </div>
      )}
      {local?.joined && paused && (
        <div className="notice">
          <Icon name="shield" />
          <div>
            <strong>{t("action.paused")}</strong>
            <p>{t("overview.pausedNotice")}</p>
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
                {pending === "pause_access"
                  ? t("common.resuming")
                  : t("overview.resume")}
              </button>
            )}
          </div>
        </div>
      )}
      {local?.joined && (
        <section className="panel network-summary">
          <div className="panel-heading">
            <h2>{t("overview.networkInfo")}</h2>
            <span className="muted">
              {network ? t("overview.encrypted") : t("common.unavailable")}
            </span>
          </div>
          {network && (
            <div className="network-facts">
              <div>
                <span>{t("common.manager")}</span>
                <strong>{network.manager_name}</strong>
              </div>
              <div>
                <span>{t("overview.relay")}</span>
                <strong>
                  {network.relay_addresses.map(relayHost).join(" · ")}
                </strong>
              </div>
            </div>
          )}
          <details className="overview-details">
            <summary>{t("overview.details")}</summary>
            <div className="panel-row">
              <span>{t("common.deviceId")}</span>
              <code>{local.device_id}</code>
            </div>
            {network && (
              <div className="panel-row">
                <span>{t("overview.relayAddresses")}</span>
                <div className="relay-addresses">
                  {network.relay_addresses.map((address) => (
                    <code key={address}>{address}</code>
                  ))}
                </div>
              </div>
            )}
            <div className="panel-row">
              <span>{t("overview.serviceInstall")}</span>
              <span>
                {service?.development
                  ? t("overview.development")
                  : service?.legacy_installed
                    ? t("overview.cliService")
                    : service?.installed
                      ? t("overview.appService")
                      : t("overview.noService")}
              </span>
            </div>
            <div className="details-actions">
              <button
                className="text-button"
                onClick={() => navigate("settings")}
              >
                {t("service.settings")}
              </button>
              {local.daemon_running && (
                <button
                  className="danger-button"
                  disabled={busy}
                  onClick={() => void stop()}
                >
                  {pending === "stop"
                    ? t("common.stopping")
                    : t("service.stopBackground")}
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
      <TrafficPanel
        status={status}
        active={active && !busy}
        devices={devices}
      />
      {local?.joined && (
        <>
          <div className="quick-links">
            <button onClick={() => navigate("devices")}>
              <Icon name="shield" />
              <span>
                <strong>{t("overview.whoHasAccess")}</strong>
                <small>{t("overview.devicesShortcut")}</small>
              </span>
              <Icon name="arrow" className="arrow" />
            </button>
            <button onClick={() => navigate("settings")}>
              <Icon name="settings" />
              <span>
                <strong>{t("settings.execution")}</strong>
                <small>{t("overview.environmentShortcut")}</small>
              </span>
              <Icon name="arrow" className="arrow" />
            </button>
          </div>
          <button
            className="history-shortcut"
            onClick={() => navigate("history")}
          >
            <Icon name="terminal" />
            {t("overview.historyShortcut")}
            <Icon name="arrow" className="arrow" />
          </button>
          <p className="footnote">{t("overview.footnote")}</p>
        </>
      )}
    </>
  );
}
