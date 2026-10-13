import { useEffect, useRef, useState } from "react";
import {
  api,
  type Device,
  type ScreenshotDownload,
  type ScreenshotFormat,
  type ScreenshotOptions,
} from "../../api";
import { useOperations } from "../../app/useOperations";
import { ErrorNotice } from "../../components/ErrorNotice";
import { Select } from "../../components/Select";
import { errorText } from "../../errors";
import { t } from "../../i18n";
import { Icon } from "../../components/Icon";

export function ScreenshotDialog({
  device,
  close,
}: {
  device: Device;
  close: () => void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const pending = useRef(false);
  const { busy: otherBusy } = useOperations();
  const [preset, setPreset] = useState("original");
  const [format, setFormat] = useState<ScreenshotFormat>("webp");
  const [width, setWidth] = useState("1600");
  const [quality, setQuality] = useState("80");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const [result, setResult] = useState<ScreenshotDownload | null>(null);
  useEffect(() => {
    dialog.current?.showModal();
  }, []);
  const options: ScreenshotOptions =
    preset === "original"
      ? { format: "png", max_width: null, quality: null }
      : preset === "compact"
        ? { format: "jpeg", max_width: 1600, quality: 80 }
        : {
            format,
            max_width: width ? Number(width) : null,
            quality: format === "png" ? null : Number(quality),
          };
  return (
    <dialog
      className="remote-command-dialog screenshot-dialog"
      aria-label={t("capture.title", { name: device.name })}
      ref={dialog}
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) close();
      }}
    >
      <header className="dialog-heading">
        <h2>{t("capture.title", { name: device.name })}</h2>
        <button
          className="icon-button"
          onClick={close}
          disabled={busy}
          aria-label={t("common.close")}
          title={t("common.close")}
        >
          <Icon name="close" />
        </button>
      </header>
      <form
        onSubmit={async (event) => {
          event.preventDefault();
          if (pending.current || otherBusy) return;
          pending.current = true;
          setBusy(true);
          setFailure(null);
          try {
            const path = await api.chooseScreenshotDestination(options.format);
            if (path)
              setResult(
                await api.remoteScreenshot(device.device_id, path, options),
              );
          } catch (error) {
            setFailure(errorText(error));
          } finally {
            pending.current = false;
            setBusy(false);
          }
        }}
      >
        <label htmlFor="capture-preset">{t("capture.preset")}</label>
        <Select
          id="capture-preset"
          value={preset}
          disabled={busy}
          onChange={setPreset}
          options={[
            { value: "original", label: t("capture.original") },
            { value: "compact", label: t("capture.compact") },
            { value: "custom", label: t("capture.custom") },
          ]}
        />
        {preset === "custom" && (
          <div className="capture-options">
            <label htmlFor="capture-format">{t("capture.format")}</label>
            <Select
              id="capture-format"
              value={format}
              disabled={busy}
              onChange={setFormat}
              options={[
                { value: "png", label: "PNG" },
                { value: "jpeg", label: "JPEG" },
                { value: "webp", label: "WebP" },
              ]}
            />
            <label htmlFor="capture-width">{t("capture.width")}</label>
            <input
              id="capture-width"
              type="number"
              min="1"
              max="16384"
              value={width}
              disabled={busy}
              onChange={(event) => setWidth(event.target.value)}
            />
            {format !== "png" && (
              <>
                <label htmlFor="capture-quality">{t("capture.quality")}</label>
                <input
                  id="capture-quality"
                  type="number"
                  min="1"
                  max="100"
                  required
                  value={quality}
                  disabled={busy}
                  onChange={(event) => setQuality(event.target.value)}
                />
              </>
            )}
          </div>
        )}
        <p className="field-help">{t("capture.hint")}</p>
        <button
          className="primary"
          disabled={busy || otherBusy || !device.online || device.revoked}
        >
          {t(busy ? "capture.capturing" : "capture.save")}
        </button>
      </form>
      {failure && (
        <ErrorNotice
          title={t("capture.failed")}
          detail={failure}
          dismiss={() => setFailure(null)}
        />
      )}
      {result && (
        <section className="capture-result">
          <p>{t("capture.saved")}</p>
          <p>{result.path}</p>
          <small>
            {result.width} × {result.height} · {result.format.toUpperCase()} ·{" "}
            {Math.ceil(result.size / 1024)} KB
          </small>
          {result.preview && (
            <img src={result.preview} alt={t("capture.preview")} />
          )}
        </section>
      )}
    </dialog>
  );
}
