import { isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useEffect, useRef, type RefObject } from "react";
import { api, type DesktopPage } from "../api";
import type { InteractionState } from "./useOperations";

export function useLightweight(
  page: DesktopPage,
  interactionState: RefObject<InteractionState>,
) {
  const currentPage = useRef(page);
  currentPage.current = page;
  useEffect(() => {
    if (!isTauri()) return;
    let active = true;
    const subscription = listen<number>(
      "xrun-lightweight-request",
      ({ payload: generation }) => {
        const state = interactionState.current;
        if (
          !active ||
          state.busy ||
          state.confirming ||
          state.windowGuards.size ||
          api.hasOpenDialog()
        )
          return;
        // The native side rechecks generation, visibility, operations and dialogs.
        // A failed acknowledgement is retried by the native idle timer.
        void api.releaseWindow(generation, currentPage.current).catch(() => {});
      },
    )
      .then((unlisten) => {
        if (!active) unlisten();
        return unlisten;
      })
      .catch(() => () => {});
    return () => {
      active = false;
      void subscription.then((unlisten) => unlisten());
    };
  }, [interactionState]);
}
