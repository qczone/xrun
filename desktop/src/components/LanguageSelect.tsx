import { useState } from "react";
import {
  changeLanguage,
  t,
  useLanguage,
  type LanguagePreference,
} from "../i18n";
import { useOperations } from "../app/useOperations";

export function LanguageSelect() {
  const { preference } = useLanguage();
  const { busy, reportError, clearError } = useOperations();
  const [saving, setSaving] = useState(false);
  return (
    <label className="setting-row" htmlFor="language">
      <span>
        <strong>{t("language.title")}</strong>
        <small id="language-hint">{t("language.hint")}</small>
      </span>
      <select
        id="language"
        aria-label={t("language.title")}
        aria-describedby="language-hint"
        value={preference}
        disabled={busy || saving}
        onChange={async (event) => {
          const selected = event.target.value as LanguagePreference;
          setSaving(true);
          clearError();
          try {
            await changeLanguage(selected);
          } catch (error) {
            reportError(error, "language.saveFailed");
          } finally {
            setSaving(false);
          }
        }}
      >
        <option value="system">{t("language.system")}</option>
        <option value="zh" lang="zh-CN">
          中文
        </option>
        <option value="en" lang="en">
          English
        </option>
      </select>
    </label>
  );
}
