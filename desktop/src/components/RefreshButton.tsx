import { t } from "../i18n";
import { Icon } from "./Icon";

export function RefreshButton({
  refreshing,
  disabled = false,
  compact = false,
  onClick,
}: {
  refreshing: boolean;
  disabled?: boolean;
  compact?: boolean;
  onClick: () => void;
}) {
  return (
    <button
      className={`refresh-button ${compact ? "icon-button" : "subtle"}`}
      aria-label={t("common.refresh")}
      aria-busy={refreshing}
      aria-disabled={disabled || refreshing}
      title={t(refreshing ? "common.refreshing" : "common.refresh")}
      data-refreshing={refreshing}
      disabled={disabled}
      onClick={() => {
        if (!disabled && !refreshing) onClick();
      }}
    >
      <Icon name="refresh" />
      {!compact && t("common.refresh")}
    </button>
  );
}
