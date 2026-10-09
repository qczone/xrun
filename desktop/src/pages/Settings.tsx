import { useOperations } from "../app/useOperations";
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
  type Status,
} from "../api";
import { Icon } from "../components/Icon";
import { ErrorNotice } from "../components/ErrorNotice";
import { LanguageSelect } from "../components/LanguageSelect";
import { Logo } from "../components/Logo";
import { errorText, osName, serviceState } from "../format";
import { t } from "../i18n";

interface Props {
  status: Status | null;
  feedback: ReactNode;
}

export function SettingsPage({ status, feedback }: Props) {
  const {
    busy,
    pending,
    action,
    stop,
    confirm,
    notify,
    reportError: onError,
  } = useOperations();
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
      const value = await api.settings();
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
  const disabled = busy || !joined || !settings;
  const [badge, serviceTone] = serviceState(status);
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
      <div className="settings-section">
        <h2>{t("settings.startup")}</h2>
        <section className="panel">
          <LanguageSelect />
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
        </section>
      </div>
      <div className="settings-section">
        <h2>{t("settings.activity")}</h2>
        <form
          className="panel padded"
          id="retention-form"
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
          <div className="concurrency-row">
            <div>
              <label htmlFor="attachment-retention">
                {t("settings.retentionDays")}
              </label>
              <p className="field-help">{t("settings.retentionHint")}</p>
            </div>
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
          </div>
          <p className="field-help">{t("settings.retentionCleanupHint")}</p>
          <div className="form-footer">
            <span
              className={
                settings &&
                Number(retention) !== settings.attachment_retention_days
                  ? "dirty"
                  : ""
              }
            >
              {settings &&
              Number(retention) !== settings.attachment_retention_days
                ? t("settings.unsaved")
                : t("settings.retentionSavedHint")}
            </span>
            <button
              type="submit"
              className="primary"
              disabled={
                busy ||
                !settings ||
                Number(retention) === settings.attachment_retention_days
              }
            >
              {pending === "save_attachment_retention"
                ? t("common.saving")
                : t("settings.saveRetention")}
            </button>
          </div>
        </form>
      </div>
      <div className="settings-section">
        <h2>{t("settings.execution")}</h2>
        {loadError && (
          <ErrorNotice
            title={t("settings.loadFailed")}
            detail={loadError}
            retry={() => void loadSettings()}
          />
        )}
        <form
          id="execution-form"
          className="panel padded"
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
            <span className="field-tag">{t("settings.newTasks")}</span>
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
      </div>
      <div className="settings-section">
        <h2>{t("service.title")}</h2>
        <section className="panel">
          <div className="setting-row">
            <span>
              <strong>
                {t("settings.serviceState")}
                <span className={`inline-status ${serviceTone}`}>{badge}</span>
              </strong>
              <small>
                {service?.development
                  ? t("settings.developmentServiceHint")
                  : t("settings.serviceHint")}
              </small>
            </span>
            <button
              className="subtle"
              disabled={busy || !joined}
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
            <div className="inline-notice">{t("settings.legacyHint")}</div>
          )}
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
                      { label: t("settings.removeService"), tone: "danger" },
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
        </section>
      </div>
      <div className="settings-section">
        <h2>{t("settings.about")}</h2>
        <section className="panel">
          <div className="setting-row">
            <div className="about-brand">
              <Logo />
              <small>{t("settings.tagline")}</small>
            </div>
            <span className="mono muted">{local?.version}</span>
          </div>
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
        </section>
      </div>
    </>
  );
}
