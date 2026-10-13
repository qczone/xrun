import type { DeploymentProgress as Progress } from "../api";
import { t } from "../i18n";

export function DeploymentProgress({
  current,
  events,
  seconds,
}: {
  current?: Progress;
  events: Progress[];
  seconds: number;
}) {
  if (!events.length) return null;
  return (
    <div className="deployment-progress">
      {current && (
        <p role="status" className="setup-help">
          {t(`deployment.${current.stage}`)} ·{" "}
          {t("deployment.elapsed", { seconds })}
        </p>
      )}
      <details className="setup-advanced">
        <summary>{t("deployment.details")}</summary>
        <ol className="deployment-steps">
          {events
            .filter(
              (event) =>
                event.status === "completed" && event.stage !== "complete",
            )
            .map((event) => (
              <li key={`${event.operation_id}:${event.sequence}`}>
                <span>{t(`deployment.${event.stage}`)}</span>
                <span>{(event.stage_elapsed_ms / 1000).toFixed(1)} s</span>
              </li>
            ))}
        </ol>
      </details>
    </div>
  );
}
