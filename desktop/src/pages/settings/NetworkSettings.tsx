import { useEffect, useRef } from "react";
import type { Status } from "../../api";
import { useOperations } from "../../app/useOperations";
import { serviceState } from "../../format";
import { t } from "../../i18n";
import { Icon } from "../../components/Icon";
import { Identifier } from "../../components/Identifier";
import { CloudflareRelays } from "../../components/CloudflareRelays";

export function NetworkSettings({
  status,
  openRequest,
}: {
  status: Status | null;
  openRequest: number;
}) {
  const { busy, pending, action, stop, confirm, notify } = useOperations();
  const details = useRef<HTMLDetailsElement>(null);
  const [serviceLabel, serviceTone] = serviceState(status);
  const local = status?.local;
  const service = status?.service;
  const network = status?.network;
  useEffect(() => {
    if (openRequest && details.current) {
      details.current.open = true;
      details.current.scrollIntoView?.({ block: "nearest" });
    }
  }, [openRequest]);
  return (
    <details
      className="settings-disclosure"
      id="network-settings"
      ref={details}
    >
      <summary>
        <span>{t("settings.networkService")}</span>
        <small>{t("settings.networkSummary")}</small>
        <Icon name="arrow" />
      </summary>
      <div className="settings-disclosure-content">
        <CloudflareRelays
          networkId={network?.network_id}
          manager={!!network?.is_manager}
        />
        {network && (
          <dl className="network-facts">
            <dt>{t("common.manager")}</dt>
            <dd>{network.manager_name}</dd>
            <dt>{t("network.relays")}</dt>
            <dd>
              {network.relay_addresses.join("\n") || t("common.unavailable")}
            </dd>
            <dt>{t("network.id")}</dt>
            <dd>
              <Identifier
                key={network.network_id}
                value={network.network_id}
                label={t("network.id")}
              />
            </dd>
            <dt>{t("common.deviceId")}</dt>
            <dd>
              {local?.device_id && (
                <Identifier
                  key={local.device_id}
                  value={local.device_id}
                  label={t("common.deviceId")}
                />
              )}
            </dd>
          </dl>
        )}
        <div className="setting-row">
          <div>
            <strong>{t("devices.allowAll")}</strong>
            <small>
              {t(
                network?.is_manager
                  ? "devices.managerProtected"
                  : "devices.allowAllHint",
              )}
            </small>
          </div>
          <label className="switch">
            <input
              type="checkbox"
              aria-label={t("devices.allowAll")}
              disabled={busy || !!network?.is_manager || !local?.joined}
              checked={!network?.is_manager && !!local?.allow_all}
              onChange={async (event) => {
                const allow = event.target.checked;
                if (
                  allow &&
                  !(await confirm(
                    t("devices.allowAllTitle"),
                    t("devices.allowAllMessage"),
                    {
                      label: t("devices.allowAllConfirm"),
                    },
                  ))
                )
                  return;
                if (
                  await action({ command: "all_permissions", args: { allow } })
                )
                  notify(allow ? "devices.allowAllOn" : "devices.allowAllOff");
              }}
            />
            <span className="switch-track" />
          </label>
        </div>
        <div className="setting-row">
          <span>
            <strong>
              {t("settings.serviceState")}
              <span className={`inline-status ${serviceTone}`}>
                {serviceLabel}
              </span>
            </strong>
            <small>
              {t(
                service?.development
                  ? "settings.developmentServiceHint"
                  : "settings.serviceHint",
              )}
            </small>
          </span>
          <button
            disabled={busy || !local?.joined}
            onClick={() =>
              local?.daemon_running
                ? void stop()
                : void action({ command: "start" })
            }
          >
            {pending === "start"
              ? t("common.starting")
              : pending === "stop"
                ? t("common.stopping")
                : local?.daemon_running
                  ? t("service.stop")
                  : t("service.start")}
          </button>
        </div>
        {service?.legacy_installed && !service.development && (
          <p className="inline-notice">{t("settings.legacyHint")}</p>
        )}
        <div className="setting-row">
          <span>
            <strong>{t("network.serviceInstall")}</strong>
            <small>
              {t(
                service?.development
                  ? "network.development"
                  : service?.legacy_installed
                    ? "network.cliService"
                    : local?.daemon_installed
                      ? "network.appService"
                      : "network.noService",
              )}
            </small>
          </span>
        </div>
        {local?.daemon_installed && (
          <div className="setting-row">
            <span>
              <strong>{t("settings.removeService")}</strong>
              <small>{t("settings.removeHint")}</small>
            </span>
            <button
              className="danger-button"
              disabled={busy}
              onClick={async () => {
                if (
                  await confirm(
                    t("settings.removeTitle"),
                    t("settings.removeMessage"),
                    {
                      label: t("settings.removeService"),
                      tone: "danger",
                    },
                  )
                )
                  await action({ command: "remove_service" });
              }}
            >
              {pending === "remove_service"
                ? t("common.removing")
                : t("settings.remove")}
            </button>
          </div>
        )}
      </div>
    </details>
  );
}
