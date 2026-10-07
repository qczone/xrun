import { t } from "../../i18n";
import { useEffect, useRef, useState } from "react";
import { api, type Revocation } from "../../api";
import type { useDevices } from "../../app/useDevices";
import { useOperations } from "../../app/useOperations";

export function useMembership(members: ReturnType<typeof useDevices>) {
  const { operate } = useOperations();
  const [revocation, setRevocation] = useState<Revocation | null>(null);
  const generation = useRef(0);
  useEffect(() => {
    generation.current++;
    setRevocation(null);
    return () => {
      generation.current++;
    };
  }, [members.scope]);
  const revoke = async (device: string) => {
    const token = generation.current;
    const network = members.scope;
    const result = await operate(
      async () => {
        const result = await api.revoke(device);
        if (!result.revoked) throw t("revoke.unconfirmed");
        members.applyRevocation(result, network);
        await members.refresh();
        return result;
      },
      { name: "revoke", title: "revoke.failed" },
    );
    if (result && token === generation.current) setRevocation(result);
  };
  return { revocation, revoke, dismiss: () => setRevocation(null) };
}
