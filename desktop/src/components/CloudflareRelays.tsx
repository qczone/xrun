import { useEffect, useState } from "react";
import { api, type RelayDeployment } from "../api";
import { useOperations } from "../app/useOperations";
import { errorText } from "../errors";
import { relayHost } from "../format";
import { t } from "../i18n";
import { ErrorNotice } from "./ErrorNotice";

export function CloudflareRelays({
  networkId,
  manager,
}: {
  networkId?: string;
  manager: boolean;
}) {
  const { busy, operate, notify } = useOperations();
  const [relays, setRelays] = useState<RelayDeployment[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [remember, setRemember] = useState(false);
  const [authorizing, setAuthorizing] = useState(false);
  useEffect(() => {
    let active = true;
    void api
      .cloudflareRelays()
      .then((result) => {
        if (active) {
          setRelays(result || []);
          setError(null);
        }
      })
      .catch((error) => {
        if (active) setError(errorText(error));
      });
    return () => {
      active = false;
    };
  }, [networkId, revision]);
  useEffect(() => {
    let current = true;
    if (manager)
      void api
        .cloudflareRememberedLogin()
        .then((value) => {
          if (current) setRemember(value);
        })
        .catch((error) => {
          if (current) setError(errorText(error));
        });
    return () => {
      current = false;
    };
  }, [manager, networkId, revision]);
  if (!relays.length && !error && !manager) return null;
  return (
    <div className="cloudflare-relays">
      <h3>{t("cloudflare.savedRelays")}</h3>
      <p className="field-help">{t("cloudflare.temporaryHint")}</p>
      {manager && (
        <>
          <label className="invitation-option">
            <input
              type="checkbox"
              checked={remember}
              disabled={busy}
              onChange={(event) => setRemember(event.target.checked)}
            />
            {t("cloudflare.rememberLogin")}
          </label>
          <div className="hero-actions">
            <button
              disabled={busy}
              onClick={async () => {
                const result = await operate(
                  async () => {
                    setAuthorizing(true);
                    try {
                      const available = await api.cloudflareAccounts(
                        () => {},
                        true,
                        remember,
                      );
                      if (available && !remember)
                        await api.cancelCloudflareAuthorization();
                      return available;
                    } finally {
                      setAuthorizing(false);
                    }
                  },
                  {
                    name: "cloudflare_accounts",
                    title: "cloudflare.authorizationFailed",
                  },
                );
                if (result) {
                  notify("cloudflare.authorized");
                  setRevision((value) => value + 1);
                }
              }}
            >
              {t("cloudflare.authorizeAccount")}
            </button>
            <button
              disabled={busy}
              onClick={async () => {
                if (
                  await operate(
                    async () => {
                      await api.logoutCloudflare();
                      return true;
                    },
                    {
                      name: "logout_cloudflare",
                      title: "cloudflare.logoutFailed",
                    },
                  )
                ) {
                  setRemember(false);
                  notify("cloudflare.loggedOut");
                  setRevision((value) => value + 1);
                }
              }}
            >
              {t("cloudflare.logout")}
            </button>
            {authorizing && (
              <button
                onClick={() =>
                  void api
                    .openCloudflareAuthorization()
                    .catch((error) => setError(errorText(error)))
                }
              >
                {t("cloudflare.openBrowser")}
              </button>
            )}
            {authorizing && (
              <button
                onClick={() =>
                  void api
                    .cancelCloudflareAuthorization()
                    .catch((error) => setError(errorText(error)))
                }
              >
                {t("cloudflare.cancel")}
              </button>
            )}
          </div>
        </>
      )}
      {error && (
        <ErrorNotice
          title={t("cloudflare.listFailed")}
          detail={error}
          retry={() => setRevision((value) => value + 1)}
        />
      )}
      {relays.map((relay) => (
        <div className="setting-row" key={relay.name}>
          <span>
            <strong>{relay.name}</strong>
            <small>{relayHost(relay.link)}</small>
          </span>
          <button
            disabled={busy}
            aria-label={t("cloudflare.copyNamed", { name: relay.name })}
            onClick={async () => {
              const copied = await operate(
                async () => {
                  await api.copyIdentifier(relay.link);
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
      ))}
    </div>
  );
}
