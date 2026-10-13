import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type Device,
  type FileTransfer,
  type TransferReceipt,
} from "../../api";
import { useOperations } from "../../app/useOperations";
import { ErrorNotice } from "../../components/ErrorNotice";
import { errorText } from "../../errors";
import { t } from "../../i18n";
export function FileDialog({
  device,
  close,
}: {
  device: Device;
  close: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const pending = useRef(false);
  const { busy: otherBusy } = useOperations();
  const [direction, setDirection] = useState<"upload" | "download">("upload");
  const [remote, setRemote] = useState("");
  const [mkdir, setMkdir] = useState(false);
  const [replace, setReplace] = useState(false);
  const [receipts, setReceipts] = useState<TransferReceipt[]>([]);
  const [result, setResult] = useState<FileTransfer | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const refresh = useCallback(
    async () => setReceipts(await api.fileReceipts(device.device_id)),
    [device.device_id],
  );
  useEffect(() => {
    dialog.current?.showModal();
    void refresh().catch((error) => setFailure(errorText(error)));
  }, [refresh]);
  const operate = async (work: () => Promise<FileTransfer | null>) => {
    if (pending.current || otherBusy) return;
    pending.current = true;
    setBusy(true);
    setFailure(null);
    setResult(null);
    try {
      const value = await work();
      if (value) setResult(value);
    } catch (error) {
      setFailure(errorText(error));
    } finally {
      try {
        await refresh();
      } catch (error) {
        setFailure((previous) => previous || errorText(error));
      }
      pending.current = false;
      setBusy(false);
    }
  };
  const disabled = busy || otherBusy || !device.online || device.revoked;
  return (
    <dialog
      ref={dialog}
      className="remote-command-dialog"
      aria-label={t("files.title", { name: device.name })}
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) close();
      }}
    >
      <header className="dialog-heading">
        <h2>{t("files.title", { name: device.name })}</h2>
        <button disabled={busy} onClick={close}>
          {t("common.close")}
        </button>
      </header>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void operate(async () => {
            const upload = direction === "upload";
            const path = await api.chooseTransferFile(
              upload,
              upload ? "" : remote.split(/[\\/]/).pop() || "download",
            );
            if (!path) return null;
            return upload
              ? api.fileUpload(device.device_id, path, {
                  path: remote,
                  cwd: null,
                  mkdir,
                  no_overwrite: !replace,
                  expect: null,
                })
              : api.fileDownload(device.device_id, remote, path, null);
          });
        }}
      >
        <div
          className="file-direction"
          role="radiogroup"
          aria-label={t("files.direction")}
        >
          {(["upload", "download"] as const).map((value) => (
            <label key={value}>
              <input
                type="radio"
                checked={direction === value}
                disabled={busy}
                onChange={() => setDirection(value)}
              />
              {t(value === "upload" ? "files.upload" : "files.download")}
            </label>
          ))}
        </div>
        <label htmlFor="transfer-remote">{t("files.remote")}</label>
        <input
          id="transfer-remote"
          required
          value={remote}
          disabled={busy}
          onChange={(event) => setRemote(event.target.value)}
        />
        {direction === "upload" && (
          <div className="file-options">
            <label>
              <input
                type="checkbox"
                checked={mkdir}
                disabled={busy}
                onChange={(event) => setMkdir(event.target.checked)}
              />
              {t("files.mkdir")}
            </label>
            <label>
              <input
                type="checkbox"
                checked={replace}
                disabled={busy}
                onChange={(event) => setReplace(event.target.checked)}
              />
              {t("files.replace")}
            </label>
          </div>
        )}
        <button className="primary" disabled={disabled || !remote.trim()}>
          {t(
            busy
              ? "files.transferring"
              : direction === "upload"
                ? "files.chooseUpload"
                : "files.chooseDownload",
          )}
        </button>
      </form>
      {failure && (
        <ErrorNotice
          title={t("files.failed")}
          detail={failure}
          dismiss={() => setFailure(null)}
        />
      )}
      {result && (
        <section className="capture-result">
          <p>{t(result.skipped ? "files.skipped" : "files.saved")}</p>
          <p>{result.path || result.remote_path}</p>
          <small>{Math.ceil(result.size / 1024)} KB</small>
        </section>
      )}
      {receipts.length > 0 && (
        <section className="file-recovery">
          <h3>{t("files.pending")}</h3>
          <p className="field-help">{t("files.resumeHint")}</p>
          {receipts.map((receipt) => (
            <div key={receipt.request_id} className="file-recovery-row">
              <span>{receipt.label}</span>
              <button
                disabled={disabled}
                onClick={() =>
                  void operate(() =>
                    api.fileResume(device.device_id, receipt.request_id),
                  )
                }
              >
                {t("files.resume")}
              </button>
              <button
                disabled={disabled}
                onClick={() =>
                  void operate(async () => {
                    await api.fileCancel(device.device_id, receipt.request_id);
                    return null;
                  })
                }
              >
                {t("files.cancel")}
              </button>
            </div>
          ))}
        </section>
      )}
    </dialog>
  );
}
