import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useState } from "react";
import { api } from "../api";

export function useWindowVisible() {
  const [visible, setVisible] = useState(() => !isTauri() && !document.hidden);
  useEffect(() => {
    let active = true;
    let nativeVisible = !isTauri();
    let revision = 0;
    const update = () => {
      if (active) setVisible(nativeVisible && !document.hidden);
    };
    document.addEventListener("visibilitychange", update);
    const subscription = isTauri()
      ? listen<boolean>("xrun-window-visible", ({ payload }) => {
          nativeVisible = payload;
          revision++;
          update();
        })
          .then(async (unlisten) => {
            if (!active) {
              unlisten();
              return () => {};
            }
            const before = revision;
            try {
              const current = await api.windowVisible();
              if (before === revision) {
                nativeVisible = current;
                update();
              }
            } catch {
              /* A later native visibility event can restore polling. */
            }
            return unlisten;
          })
          .catch(() => () => {})
      : Promise.resolve(() => {});
    return () => {
      active = false;
      document.removeEventListener("visibilitychange", update);
      void subscription.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);
  return visible;
}
