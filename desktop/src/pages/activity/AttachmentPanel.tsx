import { useEffect, useRef, useState } from "react";
import { api, type Attachment, type AttachmentPreview } from "../../api";
import { ErrorNotice } from "../../components/ErrorNotice";
import { Icon } from "../../components/Icon";
import { errorText, fileSize, dateTime } from "../../format";
import { t } from "../../i18n";
import { attachmentLabel } from "./operation";
interface Props {
  metadata: Attachment;
  active: boolean;
  paused: boolean;
  refreshVersion: number;
  onLoading: (value: boolean) => void;
  retry: () => void;
}
export function AttachmentPanel({
  metadata,
  active,
  paused,
  refreshVersion,
  onLoading,
  retry,
}: Props) {
  const [preview, setPreview] = useState<AttachmentPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState<string | null>(null);
  const [imageOpen, setImageOpen] = useState(false);
  const imageDialog = useRef<HTMLDialogElement>(null);
  const id = metadata.id;
  useEffect(() => {
    if (!active || paused || metadata.status !== "available")
      setImageOpen(false);
  }, [active, paused, metadata.status]);
  useEffect(() => {
    if (imageOpen && imageDialog.current && !imageDialog.current.open)
      imageDialog.current.showModal();
    else if (!imageOpen && imageDialog.current?.open)
      imageDialog.current.close();
  }, [imageOpen]);
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
  }, [id, metadata.status, active, paused, refreshVersion, onLoading]);
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
                time: dateTime(attachment.expires_at_ms),
              })}
        </p>
      )}
      {preview?.image && attachment?.status === "available" && (
        <button
          className="attachment-image"
          aria-label={t("attachment.openImage")}
          onClick={() => setImageOpen(true)}
        >
          <img
            src={preview.image}
            alt={t("attachment.imageAlt", { name: attachment.name })}
          />
          <span>{t("attachment.openImage")}</span>
        </button>
      )}
      <dialog
        className="attachment-image-dialog"
        ref={imageDialog}
        aria-label={t("attachment.imagePreview")}
        onClose={() => setImageOpen(false)}
      >
        <div className="dialog-heading">
          <h2>{attachment.name}</h2>
          <button
            className="icon-button"
            aria-label={t("common.close")}
            onClick={() => setImageOpen(false)}
          >
            <Icon name="close" />
          </button>
        </div>
        {imageOpen && preview?.image && attachment.status === "available" && (
          <img
            src={preview.image}
            alt={t("attachment.imageAlt", { name: attachment.name })}
          />
        )}
      </dialog>
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
