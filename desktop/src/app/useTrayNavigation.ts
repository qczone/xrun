import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef } from "react";
import { api, type DesktopNavigation, type DesktopPage } from "../api";

export function useTrayNavigation(navigate: (page: DesktopPage) => void) {
  const revision = useRef(0);
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    const apply = (request: DesktopNavigation | null) => {
      if (!active || !request || request.revision <= revision.current) return;
      revision.current = request.revision;
      navigate(request.page);
    };
    const subscription = listen<DesktopNavigation>(
      "xrun-desktop-navigation",
      ({ payload }) => apply(payload),
    )
      .then((unlisten) => {
        if (!active) {
          unlisten();
          return () => {};
        }
        // A tray click can arrive before the WebView subscribes to events.
        void api
          .desktopNavigation()
          .then(apply)
          .catch(() => {
            /* Later tray clicks still navigate through the event listener. */
          });
        return unlisten;
      })
      .catch(() => () => {});
    return () => {
      active = false;
      void subscription.then((unlisten) => unlisten());
    };
  }, [navigate]);
}
