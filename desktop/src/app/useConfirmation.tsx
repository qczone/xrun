import { t } from "../i18n";
import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import type { Confirm, ConfirmOptions } from "../api";
import { Icon } from "../components/Icon";
import type { InteractionState } from "./useOperations";

export function useConfirmation(interactionState: RefObject<InteractionState>) {
  const [prompt, setPrompt] = useState<{
    title: string;
    message: string;
    options: ConfirmOptions;
  } | null>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const resolve = useRef<((value: boolean) => void) | null>(null);
  const confirm: Confirm = useCallback(
    (title, message, options) => {
      if (resolve.current) return Promise.resolve(false);
      interactionState.current.confirming = true;
      setPrompt({ title, message, options });
      return new Promise((reply) => {
        resolve.current = reply;
      });
    },
    [interactionState],
  );
  useEffect(() => {
    if (prompt && dialog.current && !dialog.current.open) {
      dialog.current.returnValue = "";
      dialog.current.showModal();
    }
  }, [prompt]);
  useEffect(
    () => () => {
      resolve.current?.(false);
      resolve.current = null;
      interactionState.current.confirming = false;
    },
    [interactionState],
  );
  const settle = () => {
    const accepted = dialog.current?.returnValue === "ok";
    resolve.current?.(accepted);
    resolve.current = null;
    interactionState.current.confirming = false;
    setPrompt(null);
  };
  return { prompt, dialog, confirm, settle, active: prompt !== null };
}

export function ConfirmationDialog({
  controller,
}: {
  controller: ReturnType<typeof useConfirmation>;
}) {
  const { prompt, dialog, settle } = controller;
  return (
    <dialog
      ref={dialog}
      id="confirm"
      aria-labelledby="confirm-title"
      onClose={settle}
    >
      <form method="dialog">
        <div
          className={`dialog-icon ${prompt?.options.tone === "danger" ? "danger" : ""}`}
        >
          <Icon name="shield" />
        </div>
        <h2 id="confirm-title">{prompt?.title}</h2>
        <p>{prompt?.message}</p>
        <div className="dialog-actions">
          <button value="cancel">{t("common.cancel")}</button>
          <button
            className={
              prompt?.options.tone === "danger" ? "danger-primary" : "primary"
            }
            value="ok"
          >
            {prompt?.options.label}
          </button>
        </div>
      </form>
    </dialog>
  );
}
