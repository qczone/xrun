import { t } from "../i18n";
import { useEffect, useRef, useState } from "react";
import type { Action, PendingOperation } from "../api";
import { Icon } from "./Icon";

interface Props {
  joined: boolean;
  busy: boolean;
  pending: PendingOperation | null;
  startFailed: boolean;
  action: Action;
  onCreated: () => void;
}

export function NetworkSetup({
  joined,
  busy,
  pending,
  startFailed,
  action,
  onCreated,
}: Props) {
  const [mode, setMode] = useState<"join" | "create">("join");
  const [link, setLink] = useState("");
  const [name, setName] = useState("");
  const [retry, setRetry] = useState(false);
  const submitting = useRef(false);
  const creating = mode === "create";
  useEffect(() => {
    if (joined && startFailed) {
      setLink("");
      setRetry(false);
    }
  }, [joined, retry, startFailed]);
  if (joined && (!retry || startFailed)) return null;

  return (
    <section className="panel padded">
      {!joined && (
        <div className="setup-tabs" aria-label={t("setup.connectionMethod")}>
          {(["join", "create"] as const).map((value) => (
            <button
              key={value}
              aria-pressed={mode === value}
              className={mode === value ? "selected" : ""}
              disabled={busy}
              onClick={() => {
                setMode(value);
                setLink("");
                setRetry(false);
              }}
            >
              {value === "create"
                ? t("setup.createNew")
                : t("setup.joinExisting")}
            </button>
          ))}
        </div>
      )}
      <div className="section-title">
        <h2>{creating ? t("setup.createHere") : t("setup.joinExisting")}</h2>
        <p>{creating ? t("setup.createHint") : t("setup.joinHint")}</p>
      </div>
      <form
        className="network-setup-form"
        id={creating ? "create-network-form" : "join-form"}
        onSubmit={async (event) => {
          event.preventDefault();
          if (busy || submitting.current) return;
          submitting.current = true;
          const success = await action({
            command: creating ? "create_network" : "join",
            args: { link, name },
          });
          if (success) {
            setLink("");
            setRetry(false);
            if (creating) onCreated();
          } else if (creating) {
            // Creation may save the identity before starting the service.
            setRetry(true);
          }
          submitting.current = false;
        }}
      >
        <label htmlFor="setup-link">
          {creating ? t("setup.relayLink") : t("setup.invitationLink")}
        </label>
        <input
          id="setup-link"
          type="password"
          placeholder={creating ? t("setup.relayPlaceholder") : "xrun://…"}
          autoComplete="off"
          spellCheck={false}
          required
          disabled={busy}
          value={link}
          onChange={(event) => setLink(event.target.value)}
        />
        <label htmlFor="setup-name">{t("setup.deviceName")}</label>
        <input
          id="setup-name"
          type="text"
          placeholder={t("setup.namePlaceholder")}
          pattern="[a-z][a-z0-9-]{0,31}"
          maxLength={32}
          autoComplete="off"
          spellCheck={false}
          required
          disabled={busy}
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
        <p className="field-help">{t("setup.nameHint")}</p>
        <p className="setup-help">{t("setup.startHint")}</p>
        <div className="hero-actions">
          <button className="primary" type="submit" disabled={busy}>
            {pending === "create_network"
              ? t("setup.creating")
              : pending === "join"
                ? t("setup.joining")
                : creating
                  ? retry
                    ? t("setup.retryCreate")
                    : t("setup.create")
                  : t("setup.join")}
            <Icon name="arrow" />
          </button>
          {joined && retry && (
            <button
              type="button"
              disabled={busy}
              onClick={() => {
                setLink("");
                setRetry(false);
              }}
            >
              {t("common.close")}
            </button>
          )}
        </div>
      </form>
    </section>
  );
}
