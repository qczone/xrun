import { useEffect, useRef, useState } from "react";
import type { Device, Status } from "../api";
import { useTraffic } from "../app/useTraffic";
import { t } from "../i18n";
import { Icon } from "./Icon";
import { ErrorNotice } from "./ErrorNotice";
import { TrafficDetails, trafficSize } from "./TrafficDetails";

export function TrafficSummary({
  status,
  active,
  devices,
}: {
  status: Status | null;
  active: boolean;
  devices: Device[];
}) {
  const [open, setOpen] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);
  const traffic = useTraffic(status, active && !open);
  const manager = !!status?.network?.is_manager;
  useEffect(() => {
    setOpen(false);
  }, [status?.network?.network_id, status?.local.device_id]);
  useEffect(() => {
    if (!active) setOpen(false);
  }, [active]);
  useEffect(() => {
    if (open && dialog.current && !dialog.current.open)
      dialog.current.showModal();
    else if (!open && dialog.current?.open) dialog.current.close();
  }, [open]);
  if (!status?.local.joined || !status.network) return null;
  return (
    <section className="traffic-summary" aria-label={t("traffic.title")}>
      <div className="traffic-summary-row">
        <span className="traffic-summary-title">
          {t(manager ? "traffic.todayNetwork" : "traffic.todayDevice")}
        </span>
        {traffic.report ? (
          <>
            <span className="traffic-summary-stat">
              <Icon name="up" />
              {t(manager ? "traffic.ingress" : "traffic.sent")}
              <strong>
                {trafficSize(traffic.report.totals.ingress_bytes)}
              </strong>
            </span>
            <span className="traffic-summary-stat">
              <Icon name="down" />
              {t(manager ? "traffic.egress" : "traffic.received")}
              <strong>{trafficSize(traffic.report.totals.egress_bytes)}</strong>
            </span>
          </>
        ) : (
          !traffic.error && (
            <span className="muted">{t("traffic.loading")}</span>
          )
        )}
        <button className="text-button" onClick={() => setOpen(true)}>
          {t("traffic.details")}
          <Icon name="arrow" />
        </button>
      </div>
      {traffic.report && !traffic.report.complete && (
        <p className="footnote">{t("traffic.incomplete")}</p>
      )}
      {traffic.error && (
        <ErrorNotice
          title={t("traffic.unavailable")}
          detail={traffic.error}
          retry={() => void traffic.refresh()}
        />
      )}
      {traffic.error && traffic.report && (
        <p className="footnote">{t("traffic.stale")}</p>
      )}
      <dialog
        ref={dialog}
        className="traffic-dialog"
        aria-labelledby="traffic-dialog-title"
        onClose={() => setOpen(false)}
      >
        <div className="dialog-heading">
          <h2 id="traffic-dialog-title">{t("traffic.title")}</h2>
          <button
            className="icon-button"
            aria-label={t("common.close")}
            onClick={() => setOpen(false)}
          >
            <Icon name="close" />
          </button>
        </div>
        {open && (
          <TrafficDetails status={status} active={active} devices={devices} />
        )}
      </dialog>
    </section>
  );
}
