import { useState } from "react";
import {
  changeLanguage,
  t,
  useLanguage,
  type LanguagePreference,
} from "../i18n";
import { useOperations } from "../app/useOperations";
import { Select } from "./Select";

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
      <Select<LanguagePreference>
        id="language"
        aria-label={t("language.title")}
        aria-describedby="language-hint"
        value={preference}
        disabled={busy || saving}
        onChange={async (selected) => {
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
        options={[
          { value: "system", label: t("language.system") },
          { value: "zh", label: "中文", lang: "zh-CN" },
          { value: "en", label: "English", lang: "en" },
        ]}
      />
    </label>
  );
}
