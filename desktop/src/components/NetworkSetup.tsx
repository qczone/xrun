import { t } from "../i18n";
import { isTauri } from "@tauri-apps/api/core";
import { useEffect, useRef, useState } from "react";
import { api, type Action, type PendingOperation } from "../api";
import { Icon } from "./Icon";
import { CloudflareSetup } from "./CloudflareSetup";

interface Props {
  joined: boolean;
  busy: boolean;
  pending: PendingOperation | null;
  startFailed: boolean;
  action: Action;
  onCreated: () => void;
}
export function DeviceNameField({
  name,
  setName,
  busy,
}: {
  name: string;
  setName: (name: string) => void;
  busy: boolean;
}) {
  return (
    <>
      <label htmlFor="setup-name">{t("setup.deviceName")}</label>
      <input
        id="setup-name"
        type="text"
        placeholder={t("setup.namePlaceholder")}
        pattern="[a-z](?:[a-z0-9]|-){0,31}"
        maxLength={32}
        autoComplete="off"
        spellCheck={false}
        required
        disabled={busy}
        value={name}
        onChange={(event) => setName(event.target.value)}
      />
      <p className="field-help">{t("setup.nameHint")}</p>
    </>
  );
}
export function NetworkSetup({
  joined,
  busy,
  pending,
  startFailed,
  action,
  onCreated,
}: Props) {
  const [method, setMethod] = useState<"link" | "cloudflare">("link");
  const [link, setLink] = useState("");
  const [name, setName] = useState("");
  const [allowManager, setAllowManager] = useState(true);
  const [retry, setRetry] = useState(false);
  const submitting = useRef(false);
  const trimmedLink = link.trim();
  const joining = trimmedLink.startsWith("xrun://");
  const creating =
    trimmedLink.startsWith("xrun-relay://") ||
    trimmedLink.startsWith("https://");
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    void api
      .connectionDefaults()
      .then((suggestion) => {
        if (active && suggestion) setName((value) => value || suggestion);
      })
      .catch(() => {
        /* The name remains editable when detection is unavailable. */
      });
    return () => {
      active = false;
    };
  }, []);
  useEffect(() => {
    if (joined && startFailed) {
      setLink("");
      setRetry(false);
    }
  }, [joined, startFailed]);
  if (joined && (!retry || startFailed)) return null;
  const nameField = (
    <DeviceNameField name={name} setName={setName} busy={busy} />
  );
  return (
    <section className="network-setup">
      {!joined && (
        <div className="setup-tabs" aria-label={t("setup.connectionMethod")}>
          {(["link", "cloudflare"] as const).map((value) => (
            <button
              key={value}
              aria-pressed={method === value}
              className={method === value ? "selected" : ""}
              disabled={busy}
              onClick={() => {
                setMethod(value);
                setLink("");
                setRetry(false);
              }}
            >
              {t(value === "link" ? "setup.useLink" : "setup.useCloudflare")}
            </button>
          ))}
        </div>
      )}
      {method === "cloudflare" ? (
        <CloudflareSetup
          name={name}
          nameField={nameField}
          onCreated={() => {
            setRetry(false);
            onCreated();
          }}
          onRetry={() => setRetry(true)}
        />
      ) : (
        <>
          <p className="setup-description">{t("setup.linkHint")}</p>
          <form
            className="network-setup-form"
            id="connect-form"
            onSubmit={async (event) => {
              event.preventDefault();
              if (busy || submitting.current) return;
              submitting.current = true;
              try {
                const success = await action({
                  command: "connect",
                  args: {
                    link: trimmedLink,
                    name: name.trim(),
                    allowManager: joining && allowManager,
                  },
                });
                if (success) {
                  setLink("");
                  setRetry(false);
                  if (creating) onCreated();
                } else setRetry(true);
              } finally {
                submitting.current = false;
              }
            }}
          >
            <label htmlFor="setup-link">{t("setup.link")}</label>
            <input
              id="setup-link"
              type="password"
              placeholder="xrun://… / xrun-relay://… / https://…"
              autoComplete="off"
              spellCheck={false}
              required
              disabled={busy}
              value={link}
              onChange={(event) => {
                setLink(event.target.value);
                setRetry(false);
              }}
            />
            {(joining || creating) && (
              <p className="field-help" role="status">
                {t(joining ? "setup.willJoin" : "setup.willCreate")}
              </p>
            )}
            {nameField}
            {joining && (
              <>
                <label className="invitation-option">
                  <input
                    type="checkbox"
                    checked={allowManager}
                    disabled={busy}
                    onChange={(event) => setAllowManager(event.target.checked)}
                  />
                  {t("setup.allowManager")}
                </label>
                <p className="field-help">
                  {t(
                    allowManager
                      ? "setup.managerAccessHint"
                      : "setup.registrationHint",
                  )}
                </p>
              </>
            )}
            <p className="setup-help">{t("setup.startHint")}</p>
            <div className="hero-actions">
              <button className="primary" type="submit" disabled={busy}>
                {pending === "create_network"
                  ? t("setup.creating")
                  : pending === "join"
                    ? t("setup.joining")
                    : retry
                      ? t("setup.retryConnect")
                      : creating
                        ? t("setup.create")
                        : joining
                          ? t("setup.join")
                          : t("setup.connect")}
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
        </>
      )}
    </section>
  );
}
