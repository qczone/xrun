import { useEffect, useState } from "react";
import { api, type RelayDeployment } from "../api";
import { useOperations } from "../app/useOperations";
import { errorText } from "../errors";
import { relayHost } from "../format";
import { t } from "../i18n";
import { ErrorNotice } from "./ErrorNotice";

export function CloudflareRelays({ networkId }: { networkId?: string }) {
  const { busy, operate, notify } = useOperations();
  const [relays, setRelays] = useState<RelayDeployment[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
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
  if (!relays.length && !error) return null;
  return (
    <div className="cloudflare-relays">
      <h3>{t("cloudflare.savedRelays")}</h3>
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
