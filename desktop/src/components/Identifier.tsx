import { useState } from "react";
import { api } from "../api";
import { useOperations } from "../app/useOperations";
import { compactIdentifier } from "../format";
import { t } from "../i18n";
import { Icon } from "./Icon";

export function Identifier({ value, label }: { value: string; label: string }) {
  const { busy, operate, notify } = useOperations();
  const [copyState, setCopyState] = useState<"idle" | "copied" | "failed">(
    "idle",
  );
  const copyLabel = t("identifier.copy", { label });
  return (
    <span className="identifier-field">
      <button
        className="identifier"
        aria-label={copyLabel}
        title={copyState === "copied" ? t("identifier.copied") : copyLabel}
        disabled={busy}
        onClick={async () => {
          setCopyState("idle");
          const copied = await operate(
            async () => {
              await api.copyIdentifier(value);
              return true;
            },
            { name: "copy_identifier", title: "identifier.copyFailed" },
          );
          setCopyState(copied ? "copied" : "failed");
          if (copied) notify("identifier.copied");
        }}
      >
        <code>{compactIdentifier(value)}</code>
        <Icon name={copyState === "copied" ? "check" : "copy"} />
      </button>
      {copyState === "failed" && (
        <small className="identifier-error" role="alert">
          {t("identifier.copyFailed")}
        </small>
      )}
    </span>
  );
}
