import { t } from "../i18n";
export function ErrorNotice({
  title,
  detail,
  retry,
  dismiss,
}: {
  title: string;
  detail: string;
  retry?: () => void;
  dismiss?: () => void;
}) {
  return (
    <div className="error-notice" role="alert">
      <div className="error-heading">
        <strong>{title}</strong>
        {dismiss && (
          <button
            className="text-button"
            onClick={dismiss}
            aria-label={t("error.close")}
          >
            {t("common.close")}
          </button>
        )}
      </div>
      <details className="error-details">
        <summary>{t("error.details")}</summary>
        <pre>{detail}</pre>
      </details>
      {retry && <button onClick={retry}>{t("common.retry")}</button>}
    </div>
  );
}
