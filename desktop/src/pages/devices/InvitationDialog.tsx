import { useEffect, useRef, type ReactNode } from "react";
import type { Status } from "../../api";
import { InvitePanel } from "../../components/InvitePanel";

interface Props {
  open: boolean;
  close: () => void;
  active: boolean;
  status: Status | null;
  feedback: ReactNode;
}
export function InvitationDialog({
  open,
  close,
  active,
  status,
  feedback,
}: Props) {
  const dialog = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    if (open && dialog.current && !dialog.current.open)
      dialog.current.showModal();
    else if (!open && dialog.current?.open) dialog.current.close();
  }, [open]);
  return (
    <dialog
      ref={dialog}
      id="invite-dialog"
      className="invite-dialog"
      aria-labelledby="invite-title"
      onClose={close}
    >
      <div className="dialog-heading">
        <h2 id="invite-title">邀请新设备</h2>
        <button className="text-button" aria-label="关闭邀请" onClick={close}>
          关闭
        </button>
      </div>
      {open && (
        <>
          <div>{feedback}</div>
          <InvitePanel active={active && open} status={status} />
        </>
      )}
    </dialog>
  );
}
