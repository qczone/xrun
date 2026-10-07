import { useOperations } from "../app/useOperations";
import { useEffect, useRef, useState } from "react";
import { api, type Invitation, type Status } from "../api";
import { clockTime } from "../format";
import { t } from "../i18n";

interface Props {
  active: boolean;
  status: Status | null;
}

export function InvitePanel({ active, status }: Props) {
  const { busy, pending, operate, confirm, notify } = useOperations();
  const [allow, setAllow] = useState(false);
  const [invitation, setInvitation] = useState<
    (Invitation & { expiresAt: number }) | null
  >(null);
  const [expired, setExpired] = useState(false);
  const generation = useRef(0);
  const network = status?.network;
  const manager = !!network?.is_manager;
  useEffect(() => {
    generation.current++;
    setInvitation(null);
    setAllow(false);
    setExpired(false);
    return () => {
      generation.current++;
    };
  }, [active, manager, network?.network_id]);
  useEffect(() => {
    if (!invitation) return;
    const timer = setTimeout(
      () => {
        setInvitation(null);
        setExpired(true);
      },
      Math.max(0, invitation.expiresAt - Date.now()),
    );
    return () => clearTimeout(timer);
  }, [invitation]);
  if (!manager) return null;

  const generate = async () => {
    const request = generation.current;
    const grant = allow;
    if (
      grant &&
      !(await confirm(t("invite.confirmTitle"), t("invite.confirmMessage"), {
        label: t("invite.confirm"),
      }))
    )
      return;
    if (request !== generation.current) return;
    setInvitation(null);
    setExpired(false);
    const result = await operate(() => api.invite(grant), {
      name: "invite",
      title: "invite.failed",
    });
    if (result && request === generation.current)
      setInvitation({
        ...result,
        expiresAt: Date.now() + result.expires_in * 1000,
      });
  };
  return (
    <section className="invite-content" aria-label={t("invite.options")}>
      <div className="section-title">
        <p>{t("invite.description")}</p>
      </div>
      <label className="invitation-option">
        <input
          type="checkbox"
          checked={allow}
          disabled={busy}
          onChange={(event) => {
            setAllow(event.target.checked);
            setInvitation(null);
          }}
        />
        {t("invite.mutualAccess")}
      </label>
      <p className="field-help">
        {status?.local.allow_all
          ? t("invite.allowAllHint")
          : t("invite.registrationHint")}
      </p>
      <button
        className="primary"
        disabled={busy || !status?.local.daemon_connected}
        onClick={() => void generate()}
      >
        {pending === "invite" ? t("invite.generating") : t("invite.generate")}
      </button>
      {!status?.local.daemon_connected && (
        <p className="field-help">{t("invite.connectFirst")}</p>
      )}
      {expired && <p role="status">{t("invite.expired")}</p>}
      {invitation && (
        <div className="invitation-result">
          <label htmlFor="invitation-link">{t("invite.generatedLink")}</label>
          <textarea
            id="invitation-link"
            className="mono"
            value={invitation.link}
            readOnly
            rows={3}
            spellCheck={false}
          />
          <p className="field-help">
            {invitation.allow
              ? t("invite.mutualGranted")
              : t("invite.registrationOnly")}{" "}
            {t("invite.useBefore", { time: clockTime(invitation.expiresAt) })}
          </p>
          <div className="hero-actions">
            <button
              disabled={busy}
              onClick={async () => {
                if (Date.now() >= invitation.expiresAt) {
                  setInvitation(null);
                  setExpired(true);
                  return;
                }
                const copied = await operate(
                  async () => {
                    await api.copyInvitation(invitation.link);
                    return true;
                  },
                  {
                    name: "copy_invitation",
                    title: "invite.copyFailed",
                  },
                );
                if (copied) notify("invite.copied");
              }}
            >
              {pending === "copy_invitation"
                ? t("invite.copying")
                : t("invite.copy")}
            </button>
            <button disabled={busy} onClick={() => setInvitation(null)}>
              {t("invite.hide")}
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
