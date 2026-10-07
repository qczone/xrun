import type { Revocation } from "../../api";
import { useOperations } from "../../app/useOperations";
import { t } from "../../i18n";
interface Props {
  result: Revocation;
  manager: boolean;
  deviceName: (id: string) => string;
  dismiss: () => void;
  retry: () => Promise<void>;
}
export function RevocationResult({
  result,
  manager,
  deviceName,
  dismiss,
  retry,
}: Props) {
  const { busy, pending } = useOperations();
  return (
    <section className="panel padded revocation-result" role="status">
      <div className="result-heading">
        <h2>{t("revoke.result", { name: deviceName(result.device_id) })}</h2>
        <button
          className="text-button"
          onClick={dismiss}
          aria-label={t("revoke.close")}
        >
          {t("common.close")}
        </button>
      </div>
      <p>{t("revoke.saved")}</p>
      {result.sync_error && (
        <p className="warning-text">
          {t("revoke.syncFailed", { error: result.sync_error })}
        </p>
      )}
      {result.undelivered.length > 0 ? (
        <>
          <p>{t("revoke.undelivered")}</p>
          <ul>
            {result.undelivered.map((id) => (
              <li key={id}>
                {deviceName(id)} <code>{id}</code>
              </li>
            ))}
          </ul>
          <p>{t("revoke.warning")}</p>
        </>
      ) : !result.sync_error ? (
        <p>{t("revoke.confirmed")}</p>
      ) : null}
      {(result.sync_error || result.undelivered.length > 0) && manager && (
        <button disabled={busy} onClick={() => void retry()}>
          {pending === "revoke" ? t("revoke.syncing") : t("revoke.retry")}
        </button>
      )}
    </section>
  );
}
