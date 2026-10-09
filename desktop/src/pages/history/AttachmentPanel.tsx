import { useEffect, useState } from "react";
import { api, type Attachment, type AttachmentPreview } from "../../api";
import { ErrorNotice } from "../../components/ErrorNotice";
import { Icon } from "../../components/Icon";
import { errorText, fileSize, recordTime } from "../../format";
import { t } from "../../i18n";
import { attachmentLabel } from "./operation";
interface Props {
  metadata: Attachment;
  active: boolean;
  paused: boolean;
  refresh: number;
  onLoading: (value: boolean) => void;
  retry: () => void;
}
export function AttachmentPanel({
  metadata,
  active,
  paused,
  refresh,
  onLoading,
  retry,
}: Props) {
  const [preview, setPreview] = useState<AttachmentPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const id = metadata.id;
  useEffect(() => {
    if (!active || paused || metadata.status !== "available") return;
    let disposed = false;
    queueMicrotask(() => {
      if (disposed) return;
      onLoading(true);
      void api
        .attachment(id)
        .then((value) => {
          if (!disposed) {
            setPreview(value);
            setError(null);
          }
        })
        .catch((error) => {
          if (!disposed) setError(errorText(error));
        })
        .finally(() => {
          if (!disposed) onLoading(false);
        });
    });
    return () => {
      disposed = true;
      onLoading(false);
    };
  }, [id, metadata.status, active, paused, refresh, onLoading]);
  const attachment =
    metadata.status === "available"
      ? preview?.attachment || metadata
      : metadata;
  return (
    <article className="panel attachment-panel">
      <div className="attachment-heading">
        <div>
          <h2>{t("attachment.title")}</h2>
          <p className="muted">
            {attachmentLabel(attachment)}
            {fileSize(attachment?.size ?? attachment.size)}
          </p>
        </div>
        {attachment?.status === "available" && (
          <button
            disabled={saving || !preview || !!error}
            onClick={async () => {
              setSaving(true);
              setSaved(null);
              try {
                setSaved(await api.saveAttachment(attachment.id));
                setError(null);
              } catch (error) {
                setError(errorText(error));
              } finally {
                setSaving(false);
              }
            }}
          >
            <Icon name="folder" />
            {saving ? t("common.saving") : t("attachment.save")}
          </button>
        )}
      </div>
      {attachment?.status === "available" && (
        <p className="attachment-retention muted">
          {attachment.expires_at_ms === null
            ? t("attachment.forever")
            : t("attachment.expiresAt", {
                time: recordTime(attachment.expires_at_ms),
              })}
        </p>
      )}
      {preview?.image && attachment?.status === "available" && (
        <div className="attachment-image">
          <img
            src={preview.image}
            alt={t("attachment.imageAlt", { name: attachment.name })}
          />
        </div>
      )}
      {preview?.text !== null &&
        preview?.text !== undefined &&
        attachment?.status === "available" && (
          <pre className="attachment-text">{preview.text}</pre>
        )}
      {preview &&
        attachment?.status === "available" &&
        !preview.image &&
        preview.text === null && (
          <p className="empty-state">{t("attachment.saveToView")}</p>
        )}
      {!preview && attachment?.status === "available" && !error && (
        <p className="empty-state">{t("attachment.loading")}</p>
      )}
      {attachment?.status === "expired" && (
        <p className="empty-state">{t("attachment.expiredHint")}</p>
      )}
      {attachment?.status === "missing" && (
        <p className="empty-state">{t("attachment.missingHint")}</p>
      )}
      {saved && (
        <p className="attachment-saved" role="status">
          {t("attachment.saved", { path: saved })}
        </p>
      )}
      {error && (
        <ErrorNotice
          title={t("attachment.loadFailed")}
          detail={error}
          retry={retry}
          dismiss={() => setError(null)}
        />
      )}
      {attachment && (
        <details className="attachment-facts">
          <summary>{t("attachment.details")}</summary>
          <code>SHA-256: {attachment.sha256}</code>
        </details>
      )}
    </article>
  );
}
