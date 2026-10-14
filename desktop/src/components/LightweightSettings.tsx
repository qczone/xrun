import { useCallback, useEffect, useState } from "react";
import { api, type LightweightSettings as Preferences } from "../api";
import { useOperations } from "../app/useOperations";
import { errorText } from "../errors";
import { t } from "../i18n";
import { ErrorNotice } from "./ErrorNotice";
import { Select } from "./Select";

export function LightweightSettings() {
  const { busy, operate } = useOperations();
  const [preferences, setPreferences] = useState<Preferences | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(async () => {
    try {
      setPreferences(await api.lightweightSettings());
      setError(null);
    } catch (error) {
      setError(errorText(error));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);
  const save = async (next: Preferences) => {
    await operate(
      async () => {
        setPreferences(await api.saveLightweightSettings(next));
      },
      { name: "save_lightweight_settings", title: "lightweight.saveFailed" },
    );
  };
  const delay = preferences?.delay_seconds ?? 30;
  const delays = [...new Set([0, 30, 60, 300, 600, delay])].sort(
    (a, b) => a - b,
  );
  return (
    <>
      {error && (
        <ErrorNotice
          title={t("lightweight.loadFailed")}
          detail={error}
          retry={() => void load()}
        />
      )}
      <label className="setting-row" htmlFor="auto-lightweight">
        <span>
          <strong>{t("lightweight.automatic")}</strong>
          <small id="lightweight-automatic-hint">{t("lightweight.hint")}</small>
        </span>
        <span className="switch">
          <input
            id="auto-lightweight"
            type="checkbox"
            aria-label={t("lightweight.automatic")}
            aria-describedby="lightweight-automatic-hint"
            checked={preferences?.enabled ?? false}
            disabled={busy || !preferences}
            onChange={(event) => {
              if (preferences)
                void save({ ...preferences, enabled: event.target.checked });
            }}
          />
          <span className="switch-track" />
        </span>
      </label>
      <div className="setting-row">
        <span>
          <strong>{t("lightweight.delay")}</strong>
        </span>
        <Select
          id="lightweight-delay"
          aria-label={t("lightweight.delay")}
          value={String(delay)}
          disabled={busy || !preferences?.enabled}
          options={delays.map((seconds) => ({
            value: String(seconds),
            label:
              seconds === 0
                ? t("lightweight.immediately")
                : seconds % 60 !== 0
                  ? t("lightweight.seconds", { count: seconds })
                  : seconds === 60
                    ? t("lightweight.minute")
                    : t("lightweight.minutes", { count: seconds / 60 }),
          }))}
          onChange={(value) => {
            if (preferences)
              void save({ ...preferences, delay_seconds: Number(value) });
          }}
        />
      </div>
      <div className="setting-row">
        <span>
          <strong>{t("lightweight.title")}</strong>
          <small>{t("lightweight.protectedHint")}</small>
        </span>
        <button
          className="subtle"
          disabled={busy}
          onClick={() =>
            void operate(() => api.enterLightweight(), {
              name: "enter_lightweight",
              title: "lightweight.enterFailed",
            })
          }
        >
          {t("lightweight.enter")}
        </button>
      </div>
    </>
  );
}
