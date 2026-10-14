import { useKeepWindow, useOperations } from "../app/useOperations";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  api,
  type ExecutionSettings,
  type Settings,
  type PrivateInterface,
  type Status,
} from "../api";
import { Select } from "../components/Select";
import { Icon } from "../components/Icon";
import { ErrorNotice } from "../components/ErrorNotice";
import { LanguageSelect } from "../components/LanguageSelect";
import { LightweightSettings } from "../components/LightweightSettings";
import { NetworkSettings } from "./settings/NetworkSettings";
import { errorText, osName } from "../format";
import { t } from "../i18n";

interface Props {
  status: Status | null;
  feedback: ReactNode;
  networkSettingsRequest: number;
}

export function SettingsPage({
  status,
  feedback,
  networkSettingsRequest,
}: Props) {
  const {
    busy,
    pending,
    action,
    notify,
    reportError: onError,
  } = useOperations();
  const [interfaces, setInterfaces] = useState<PrivateInterface[]>([]);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [cwd, setCwd] = useState("");
  const [concurrency, setConcurrency] = useState("4");
  const [path, setPath] = useState("");
  const [retention, setRetention] = useState("30");
  const [loadError, setLoadError] = useState<string | null>(null);
  const loadRequest = useRef(0);
  const joined = !!status?.local.joined;
  const loadSettings = useCallback(async () => {
    const request = ++loadRequest.current;
    try {
      const [value, adapters] = await Promise.all([
        api.settings(),
        api.privateInterfaces(),
      ]);
      setInterfaces(adapters);
      if (request !== loadRequest.current) return;
      setSettings(value);
      setCwd(value.execution.default_cwd || "");
      setConcurrency(String(value.execution.max_concurrent_jobs));
      setPath(value.execution.path || "");
      setRetention(String(value.attachment_retention_days));
      setLoadError(null);
    } catch (error) {
      if (request === loadRequest.current) setLoadError(errorText(error));
    }
  }, []);

  useEffect(() => {
    void loadSettings();
    return () => {
      loadRequest.current++;
    };
    // Status polling must not overwrite the user's unsaved inputs.
  }, [joined, loadSettings]);

  const execution: ExecutionSettings = {
    default_cwd: cwd.trim() ? cwd : null,
    max_concurrent_jobs: Number(concurrency),
    path: path.trim() ? path : null,
  };
  const dirty =
    settings !== null &&
    JSON.stringify(execution) !== JSON.stringify(settings.execution);
  useKeepWindow(
    "settings-draft",
    dirty ||
      (settings !== null &&
        Number(retention) !== settings.attachment_retention_days),
  );
  const disabled = busy || !joined || !settings;
  const local = status?.local;
  const service = status?.service;

  return (
    <>
      <div className="page-heading">
        <div>
          <h1>{t("nav.settings")}</h1>
          <p>{t("settings.description")}</p>
        </div>
      </div>
      {feedback}
      {loadError && (
        <ErrorNotice
          title={t("settings.loadFailed")}
          detail={loadError}
          retry={() => void loadSettings()}
        />
      )}
      <section className="settings-section">
        <h2>{t("settings.general")}</h2>
        <div className="settings-card">
          <LanguageSelect />
          <LightweightSettings />
          <div className="setting-row">
            <span>
              <strong>{t("settings.direct")}</strong>
              <small>{t("settings.directHint")}</small>
            </span>
            <Select
              id="direct-interface"
              aria-label={t("settings.direct")}
              value={settings?.lan_address || "off"}
              disabled={busy || !settings}
              options={[
                { value: "off", label: t("settings.directOff") },
                ...interfaces.map((adapter) => ({
                  value: adapter.address,
                  label: `${adapter.address} · ${adapter.name}`,
                })),
              ]}
              onChange={async (value) => {
                const address = value === "off" ? null : value;
                if (
                  await action({
                    command: "save_direct_connection",
                    args: { address },
                  })
                )
                  setSettings(
                    (previous) =>
                      previous && { ...previous, lan_address: address },
                  );
              }}
            />
          </div>
          <div className="setting-row">
            <span>
              <strong>{t("settings.compression")}</strong>
              <small>{t("settings.compressionHint")}</small>
            </span>
            <Select
              id="traffic-compression"
              aria-label={t("settings.compression")}
              value={settings?.traffic_compression || "auto"}
              disabled={busy || !settings}
              options={[
                { value: "auto", label: t("settings.compressionAuto") },
                { value: "none", label: t("settings.compressionNone") },
                { value: "zstd", label: "Zstd" },
              ]}
              onChange={async (value) => {
                const compression = value as Settings["traffic_compression"];
                if (
                  await action({
                    command: "save_traffic_compression",
                    args: { compression },
                  })
                )
                  setSettings(
                    (previous) =>
                      previous && {
                        ...previous,
                        traffic_compression: compression,
                      },
                  );
              }}
            />
          </div>
          <label className="setting-row" htmlFor="autostart">
            <span>
              <strong>{t("settings.showAtLogin")}</strong>
              <small>
                {service?.development
                  ? t("settings.developmentLoginHint")
                  : t("settings.loginHint")}
              </small>
            </span>
            <span className="switch">
              <input
                id="autostart"
                type="checkbox"
                disabled={busy || !status || service?.development}
                checked={service?.app_at_login || false}
                onChange={(event) =>
                  void action({
                    command: "autostart",
                    args: { enabled: event.target.checked },
                  })
                }
              />
              <span className="switch-track" />
            </span>
          </label>
          <div className="setting-row">
            <span>
              <strong>{t("settings.tray")}</strong>
              <small>{t("settings.trayHint")}</small>
            </span>
            <button
              className="subtle"
              disabled={busy}
              onClick={() => void action({ command: "hide_icon" })}
            >
              {pending === "hide_icon"
                ? t("common.hiding")
                : t("settings.hideIcon")}
            </button>
          </div>
        </div>
      </section>
      <section className="settings-section">
        <h2>{t("settings.activity")}</h2>
        <form
          id="retention-form"
          className="settings-card"
          onSubmit={async (event) => {
            event.preventDefault();
            const days = Number(retention);
            if (
              busy ||
              !settings ||
              days === settings.attachment_retention_days
            )
              return;
            if (
              await action({
                command: "save_attachment_retention",
                args: { days },
              })
            ) {
              setSettings(
                (previous) =>
                  previous && { ...previous, attachment_retention_days: days },
              );
              notify("settings.retentionSaved");
            }
          }}
        >
          <div className="setting-row">
            <div>
              <label htmlFor="attachment-retention">
                {t("settings.retentionDays")}
              </label>
              <small>{t("settings.retentionHint")}</small>
            </div>
            <div className="retention-controls">
              <input
                id="attachment-retention"
                type="number"
                min="0"
                max="3650"
                required
                disabled={busy || !settings}
                value={retention}
                onChange={(event) => setRetention(event.target.value)}
              />
              {settings &&
                Number(retention) !== settings.attachment_retention_days && (
                  <button className="primary" type="submit" disabled={busy}>
                    {t("settings.saveRetention")}
                  </button>
                )}
            </div>
          </div>
          {settings &&
            Number(retention) > 0 &&
            (settings.attachment_retention_days === 0 ||
              Number(retention) < settings.attachment_retention_days) && (
              <p className="field-help retention-cleanup-hint">
                {t("settings.retentionCleanupHint")}
              </p>
            )}
        </form>
      </section>
      <details
        className="settings-card settings-disclosure"
        id="execution-settings"
      >
        <summary>
          <span className="settings-icon">
            <Icon name="terminal" />
          </span>
          <span className="settings-disclosure-title">
            <span>{t("settings.execution")}</span>
            <small>{t("settings.executionSummary")}</small>
          </span>
          <Icon name="chevron-down" />
        </summary>
        <form
          id="execution-form"
          className="settings-disclosure-content"
          onSubmit={async (event) => {
            event.preventDefault();
            if (disabled || !dirty) return;
            if (
              await action({ command: "save_settings", args: { execution } })
            ) {
              // Use the saved snapshot; subsequent status reads never reset the form.
              setSettings((previous) => previous && { ...previous, execution });
              notify("settings.saved");
            }
          }}
        >
          <div className="field-heading">
            <label htmlFor="cwd">{t("settings.defaultCwd")}</label>
            <span className="field-tag">{t("settings.newJobs")}</span>
          </div>
          <div className="input-with-button">
            <input
              id="cwd"
              type="text"
              placeholder={settings?.home_dir || t("settings.homePlaceholder")}
              spellCheck={false}
              disabled={disabled}
              value={cwd}
              onChange={(event) => setCwd(event.target.value)}
            />
            <button
              type="button"
              disabled={disabled}
              onClick={async () => {
                try {
                  const selected = await api.chooseDirectory();
                  if (selected) setCwd(selected);
                } catch (e) {
                  onError(e, "settings.chooseFailed");
                }
              }}
            >
              <Icon name="folder" />
              {t("settings.choose")}
            </button>
          </div>
          <p className="field-help">{t("settings.cwdHint")}</p>
          <div className="concurrency-row">
            <div>
              <label htmlFor="concurrency">{t("settings.concurrency")}</label>
              <p className="field-help">{t("settings.concurrencyHint")}</p>
            </div>
            <input
              id="concurrency"
              type="number"
              min="1"
              max="64"
              required
              disabled={disabled}
              value={concurrency}
              onChange={(event) => setConcurrency(event.target.value)}
            />
          </div>
          <details className="advanced">
            <summary>{t("settings.pathTitle")}</summary>
            <label className="sr-only" htmlFor="path">
              {t("settings.path")}
            </label>
            <textarea
              id="path"
              rows={2}
              spellCheck={false}
              placeholder={t("settings.pathPlaceholder")}
              disabled={disabled}
              value={path}
              onChange={(event) => setPath(event.target.value)}
            />
            <p className="field-help">
              {t("settings.pathHint", {
                separator:
                  settings?.os === "windows"
                    ? t("settings.semicolon")
                    : t("settings.colon"),
              })}
            </p>
          </details>
          <div className="form-footer">
            <span className={dirty ? "dirty" : ""}>
              {dirty ? t("settings.unsaved") : t("settings.savedHint")}
            </span>
            <button
              type="submit"
              className="primary"
              disabled={disabled || !dirty}
            >
              {pending === "save_settings"
                ? t("common.saving")
                : t("settings.save")}
            </button>
          </div>
          {!joined && <p className="field-help">{t("settings.joinFirst")}</p>}
        </form>
      </details>
      <NetworkSettings status={status} openRequest={networkSettingsRequest} />
      <details className="settings-about">
        <summary>
          xrun {local?.version}
          <span>·</span>
          {t("settings.about")}
          <Icon name="chevron-down" />
        </summary>
        <div className="settings-card">
          <div className="setting-row">
            <span>
              <strong>{t("settings.system")}</strong>
              <small>
                {settings ? osName(settings.os) : t("common.loading")}
              </small>
            </span>
          </div>
          <div className="setting-row data-row">
            <span>
              <strong>{t("settings.dataDir")}</strong>
              <small>{t("settings.dataHint")}</small>
            </span>
            <input
              type="text"
              readOnly
              aria-label={t("settings.dataDir")}
              value={settings?.data_dir || ""}
              onFocus={(event) => event.target.select()}
            />
          </div>
        </div>
      </details>
    </>
  );
}
