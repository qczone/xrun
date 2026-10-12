import { useEffect, useState } from "react";
import { api } from "../api";
import { errorText } from "../errors";
import { t } from "../i18n";
import { Select } from "./Select";
import { ErrorNotice } from "./ErrorNotice";

export function MembershipFreshness() {
  const [minutes, setMinutes] = useState(15);
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    void api
      .settings()
      .then((settings) => {
        if (active) setMinutes(settings.membership_lease_minutes || 15);
      })
      .catch((error) => {
        if (active) setFailure(errorText(error));
      });
    return () => {
      active = false;
    };
  }, []);
  const choices = Array.from(new Set([5, 15, 60, 1440, minutes])).sort(
    (a, b) => a - b,
  );
  return (
    <section className="authority-settings">
      <div className="setting-row">
        <span>
          <strong>{t("freshness.title")}</strong>
          <small>{t("freshness.hint")}</small>
        </span>
        <Select
          id="membership-freshness"
          aria-label={t("freshness.title")}
          value={String(minutes)}
          disabled={busy}
          options={choices.map((value) => ({
            value: String(value),
            label: t("freshness.minutes", { minutes: value }),
          }))}
          onChange={async (value) => {
            setBusy(true);
            setFailure(null);
            try {
              await api.saveMembershipLease(Number(value));
              setMinutes(Number(value));
            } catch (error) {
              setFailure(errorText(error));
            } finally {
              setBusy(false);
            }
          }}
        />
      </div>
      {failure && (
        <ErrorNotice
          title={t("freshness.failed")}
          detail={failure}
          dismiss={() => setFailure(null)}
        />
      )}
    </section>
  );
}
