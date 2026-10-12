import { useRef, useState, type ReactNode } from "react";
import { api, type CloudflareAccount, type RelayDeployment } from "../api";
import { useOperations } from "../app/useOperations";
import { t } from "../i18n";
import { errorText } from "../errors";
import { ErrorNotice } from "./ErrorNotice";

export function CloudflareSetup({
  name,
  nameField,
  onCreated,
  onRetry,
}: {
  name: string;
  nameField: ReactNode;
  onCreated: () => void;
  onRetry: () => void;
}) {
  const { busy, operate, notify, error } = useOperations();
  const [workerName, setWorkerName] = useState("xrun-relay");
  const [accounts, setAccounts] = useState<CloudflareAccount[] | null>(null);
  const [account, setAccount] = useState("");
  const [deployment, setDeployment] = useState<RelayDeployment | null>(null);
  const [authorizationError, setAuthorizationError] = useState<string | null>(
    null,
  );
  const [phase, setPhase] = useState<
    "idle" | "accounts" | "authorization" | "deploying" | "connecting"
  >("idle");
  const submitting = useRef(false);
  const reauthorize = async () => {
    if (busy || submitting.current) return;
    setAuthorizationError(null);
    await operate(
      async () => {
        setPhase("accounts");
        try {
          const available = await api.cloudflareAccounts(
            () => setPhase("authorization"),
            true,
          );
          if (available) {
            setAccounts(available);
            setAccount(available.length === 1 ? available[0].id : "");
          }
        } finally {
          setPhase("idle");
        }
      },
      { name: "cloudflare_accounts", title: "cloudflare.authorizationFailed" },
    );
  };
  const create = async () => {
    if (busy || submitting.current) return;
    submitting.current = true;
    setAuthorizationError(null);
    const success = await operate(
      async () => {
        try {
          let selectedAccount = account;
          if (!deployment && !accounts) {
            setPhase("accounts");
            const available = await api.cloudflareAccounts(() =>
              setPhase("authorization"),
            );
            if (!available) return false;
            setAccounts(available);
            if (available.length !== 1) return false;
            selectedAccount = available[0].id;
            setAccount(selectedAccount);
          }
          if (!deployment && !selectedAccount) return false;
          let relay = deployment;
          if (!relay) {
            setPhase("deploying");
            relay = await api.deployCloudflare(
              workerName.trim(),
              selectedAccount,
            );
            setDeployment(relay);
          }
          setPhase("connecting");
          await api.action({
            command: "connect",
            args: { link: relay.link, name: name.trim(), allowManager: false },
          });
          onCreated();
          return true;
        } finally {
          setPhase("idle");
        }
      },
      { name: "deploy_cloudflare", title: "cloudflare.createFailed" },
    );
    if (success === undefined) onRetry();
    submitting.current = false;
  };
  return (
    <>
      <p className="setup-description">{t("cloudflare.description")}</p>
      <form
        className="network-setup-form"
        id="cloudflare-create-form"
        onSubmit={(event) => {
          event.preventDefault();
          void create();
        }}
      >
        {nameField}
        {!deployment && (
          <details className="setup-advanced">
            <summary>{t("cloudflare.advanced")}</summary>
            <label htmlFor="worker-name">{t("cloudflare.workerName")}</label>
            <input
              id="worker-name"
              required
              pattern="[a-z0-9][a-z0-9-]{0,62}"
              maxLength={63}
              onInvalid={(event) => {
                const details = event.currentTarget.closest("details");
                if (details) details.open = true;
              }}
              value={workerName}
              disabled={busy}
              onChange={(event) => {
                setWorkerName(event.target.value);
              }}
            />
          </details>
        )}
        {!deployment && (accounts || error) && (
          <button
            type="button"
            className="text-button"
            disabled={busy}
            onClick={() => void reauthorize()}
          >
            {t("cloudflare.reauthorize")}
          </button>
        )}
        {accounts && accounts.length > 1 && !deployment && (
          <>
            <label htmlFor="cloudflare-account">
              {t("cloudflare.account")}
            </label>
            <select
              id="cloudflare-account"
              value={account}
              required
              disabled={busy}
              onChange={(event) => setAccount(event.target.value)}
            >
              <option value="">{t("cloudflare.chooseAccount")}</option>
              {accounts.map((item) => (
                <option key={item.id} value={item.id}>
                  {item.name}
                </option>
              ))}
            </select>
          </>
        )}
        {deployment && (
          <div className="relay-deployment-result">
            <p>{t("cloudflare.deployedHint")}</p>
            <button
              type="button"
              disabled={busy}
              onClick={async () => {
                const copied = await operate(
                  async () => {
                    await api.copyIdentifier(deployment.link);
                    return true;
                  },
                  { name: "copy_identifier", title: "cloudflare.copyFailed" },
                );
                if (copied) notify("cloudflare.copied");
              }}
            >
              {t("cloudflare.copyLink")}
            </button>
          </div>
        )}
        {phase !== "idle" && (
          <p role="status" className="setup-help">
            {t(`cloudflare.${phase}`)}
          </p>
        )}
        {authorizationError && (
          <ErrorNotice
            title={t("cloudflare.authorizationFailed")}
            detail={authorizationError}
            dismiss={() => setAuthorizationError(null)}
          />
        )}
        {phase === "authorization" && (
          <div className="hero-actions">
            <button
              type="button"
              onClick={() =>
                void api
                  .openCloudflareAuthorization()
                  .catch((error) => setAuthorizationError(errorText(error)))
              }
            >
              {t("cloudflare.openBrowser")}
            </button>
            <button
              type="button"
              onClick={() =>
                void api
                  .cancelCloudflareAuthorization()
                  .catch((error) => setAuthorizationError(errorText(error)))
              }
            >
              {t("cloudflare.cancel")}
            </button>
          </div>
        )}
        <p className="setup-help">{t("cloudflare.managerHint")}</p>
        <div className="hero-actions">
          <button className="primary" type="submit" disabled={busy}>
            {busy
              ? t("cloudflare.working")
              : deployment
                ? t("setup.retryCreate")
                : t("cloudflare.create")}
          </button>
        </div>
      </form>
    </>
  );
}
