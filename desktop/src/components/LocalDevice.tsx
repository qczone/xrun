import type { Status } from "../api";
import { useOperations } from "../app/useOperations";
import { localState } from "../format";
import { t } from "../i18n";
import { Icon } from "./Icon";

/** The sidebar footer identifies this device and keeps the pause control at hand. */
export function LocalDeviceCard({
  status,
  openNetworkSettings,
}: {
  status: Status;
  openNetworkSettings: () => void;
}) {
  const { busy, action } = useOperations();
  const { local } = status;
  const [state, tone] = localState(status);
  return (
    <div className="local-device">
      <button
        className="local-device-identity"
        title={t("settings.networkService")}
        onClick={openNetworkSettings}
      >
        <span className="local-device-avatar">
          <Icon name="monitor" />
          <i className={`dot ${tone}`} />
        </span>
        <span className="local-device-text">
          <strong>{local.name || t("common.localDevice")}</strong>
          <span>
            {t(status.network?.is_manager ? "common.manager" : "common.member")}
          </span>
        </span>
      </button>
      <div className="local-device-state">
        <span
          className={`${tone} ${local.remote_access_paused ? "access-paused" : ""}`}
        >
          <i className={`dot ${tone}`} />
          {state}
        </span>
        {local.daemon_running && !local.remote_access_paused && (
          <button
            className="icon-button"
            aria-label={t("local.pauseAccess")}
            title={t("local.pauseAccess")}
            disabled={busy}
            onClick={() =>
              void action({ command: "pause_access", args: { paused: true } })
            }
          >
            <Icon name="pause" />
          </button>
        )}
      </div>
    </div>
  );
}

/** Only states that block remote access interrupt the page, each with its fix. */
export function StatusBanner({ status }: { status: Status }) {
  const { busy, pending, action } = useOperations();
  const { local, service } = status;
  const start = !local.daemon_running && (
    <button
      className="primary"
      disabled={busy}
      onClick={() => void action({ command: "start" })}
    >
      {pending === "start" ? t("common.starting") : t("service.start")}
    </button>
  );
  if (service.approval_required)
    return (
      <div className="status-banner warning" role="status">
        <Icon name="shield" />
        <div>
          <strong>{t("local.approvalTitle")}</strong>
          <p>{t("local.approvalInstructions")}</p>
        </div>
        {start}
      </div>
    );
  if (!local.daemon_running)
    return (
      <div className="status-banner" role="status">
        <Icon name="power" />
        <div>
          <strong>{t("banner.stoppedTitle")}</strong>
          <p>{t("banner.stoppedText")}</p>
        </div>
        {start}
      </div>
    );
  if (local.remote_access_paused)
    return (
      <div className="status-banner warning" role="status">
        <Icon name="pause" />
        <div>
          <strong>{t("banner.pausedTitle")}</strong>
          <p>{t("status.pausedHint")}</p>
        </div>
        <button
          disabled={busy}
          onClick={() =>
            void action({ command: "pause_access", args: { paused: false } })
          }
        >
          <Icon name="play" />
          {pending === "pause_access"
            ? t("common.updating")
            : t("local.resumeAccess")}
        </button>
      </div>
    );
  return null;
}
