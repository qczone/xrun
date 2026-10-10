import type { Device, Status, TrafficReport } from "../api";
import { useTraffic } from "../app/useTraffic";
import { recordTime } from "../format";
import { t } from "../i18n";
import { ErrorNotice } from "./ErrorNotice";

export function trafficSize(bytes: number) {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KiB", "MiB", "GiB", "TiB", "PiB"];
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value.toFixed(1)} ${units[unit]}`;
}
const utcDay = (time: number) => new Date(time).toISOString().slice(0, 10);

function Trend({ report }: { report: TrafficReport }) {
  const maximum = Math.max(
    1,
    ...report.daily.flatMap((day) => [day.ingress_bytes, day.egress_bytes]),
  );
  const width = 600 / Math.max(1, report.daily.length);
  return (
    <>
      <div className="traffic-legend">
        <span className="traffic-ingress">
          {t(report.device_id ? "traffic.sent" : "traffic.ingress")}
        </span>
        <span className="traffic-egress">
          {t(report.device_id ? "traffic.received" : "traffic.egress")}
        </span>
        {report.period === "all" && (
          <span className="muted">{t("traffic.last30Days")}</span>
        )}
      </div>
      <svg
        className="traffic-chart"
        viewBox="0 0 600 100"
        role="img"
        aria-label={t("traffic.dailyTrend")}
      >
        {report.daily.map((day, index) => (
          <g key={day.start_ms}>
            <title>{`${utcDay(day.start_ms)} · ${trafficSize(day.ingress_bytes)} / ${trafficSize(day.egress_bytes)}`}</title>
            <rect
              className="traffic-bar-ingress"
              x={index * width + width * 0.15}
              y={100 - (day.ingress_bytes / maximum) * 95}
              width={width * 0.3}
              height={(day.ingress_bytes / maximum) * 95}
            />
            <rect
              className="traffic-bar-egress"
              x={index * width + width * 0.5}
              y={100 - (day.egress_bytes / maximum) * 95}
              width={width * 0.3}
              height={(day.egress_bytes / maximum) * 95}
            />
          </g>
        ))}
      </svg>
      <div className="traffic-axis">
        <span>{report.daily[0] && utcDay(report.daily[0].start_ms)}</span>
        <span>
          {report.daily.length > 1 &&
            utcDay(report.daily[report.daily.length - 1].start_ms)}
        </span>
      </div>
    </>
  );
}

export function TrafficPanel({
  status,
  active,
  devices,
}: {
  status: Status | null;
  active: boolean;
  devices: Device[];
}) {
  const traffic = useTraffic(status, active);
  if (!status?.local.joined || !status.network) return null;
  const { report, period, loading, error } = traffic;
  const manager = status.network.is_manager;
  const names = new Map(
    devices.map((device) => [device.device_id, device.name]),
  );
  if (status.local.device_id && status.local.name)
    names.set(status.local.device_id, status.local.name);
  return (
    <section className="panel traffic-panel" aria-label={t("traffic.title")}>
      <div className="panel-heading">
        <h2>{t(manager ? "traffic.network" : "traffic.device")}</h2>
        <div
          className="traffic-periods"
          role="group"
          aria-label={t("traffic.period")}
        >
          {(["today", "month", "all"] as const).map((value) => (
            <button
              key={value}
              className={`text-button ${period === value ? "selected" : ""}`}
              aria-pressed={period === value}
              onClick={() => traffic.changePeriod(value)}
            >
              {t(`traffic.${value}`)}
            </button>
          ))}
        </div>
      </div>
      {error && (
        <ErrorNotice
          title={t("traffic.unavailable")}
          detail={error}
          retry={() => void traffic.refresh()}
        />
      )}
      {!report && !error && (
        <p className="muted traffic-note" role="status">
          {t("traffic.loading")}
        </p>
      )}
      {report && (
        <>
          {!report.complete && (
            <p className="notice traffic-note">{t("traffic.incomplete")}</p>
          )}
          {error && <p className="muted traffic-note">{t("traffic.stale")}</p>}
          <div className="traffic-totals">
            <div>
              <span>{t(manager ? "traffic.ingress" : "traffic.sent")}</span>
              <strong>{trafficSize(report.totals.ingress_bytes)}</strong>
            </div>
            <div>
              <span>{t(manager ? "traffic.egress" : "traffic.received")}</span>
              <strong>{trafficSize(report.totals.egress_bytes)}</strong>
            </div>
          </div>
          <div className="traffic-trend">
            <Trend report={report} />
          </div>
          {manager && (
            <details className="overview-details">
              <summary>{t("traffic.devices")}</summary>
              {report.devices.length ? (
                <table className="traffic-devices">
                  <thead>
                    <tr>
                      <th>{t("traffic.deviceColumn")}</th>
                      <th>{t("traffic.sent")}</th>
                      <th>{t("traffic.received")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {report.devices.map((device) => (
                      <tr key={device.device_id}>
                        <td title={device.device_id}>
                          {names.get(device.device_id) || device.device_id}
                        </td>
                        <td>{trafficSize(device.sent_bytes)}</td>
                        <td>{trafficSize(device.received_bytes)}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              ) : (
                <p className="muted">{t("traffic.empty")}</p>
              )}
              {(traffic.offset > 0 || report.next_offset !== null) && (
                <div className="traffic-pagination">
                  <button
                    disabled={loading || traffic.offset === 0}
                    onClick={() =>
                      traffic.changeOffset(Math.max(0, traffic.offset - 50))
                    }
                  >
                    {t("traffic.previous")}
                  </button>
                  <button
                    disabled={loading || report.next_offset === null}
                    onClick={() => {
                      if (report.next_offset !== null)
                        traffic.changeOffset(report.next_offset);
                    }}
                  >
                    {t("traffic.next")}
                  </button>
                </div>
              )}
            </details>
          )}
          <p className="muted traffic-note">
            {t("traffic.updated", { time: recordTime(report.end_ms) })}
          </p>
        </>
      )}
      <p className="muted traffic-note">{t("traffic.hint")}</p>
    </section>
  );
}
