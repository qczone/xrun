import { useEffect, useRef, useState } from "react";
import { api, type Device, type Status } from "../api";
import { useOperations } from "../app/useOperations";
import { Select } from "./Select";
import { ErrorNotice } from "./ErrorNotice";
import { errorText } from "../errors";
import { t } from "../i18n";
import { Icon } from "./Icon";

type AuthorityAction = "backup" | "restore" | "migrate";
export function AuthoritySettings({ status }: { status: Status | null }) {
  const { busy } = useOperations();
  const [mode, setMode] = useState<AuthorityAction | null>(null);
  const manager = !!status?.network?.is_manager;
  useEffect(() => setMode(null), [status?.network?.network_id, manager]);
  return (
    <section className="authority-settings">
      <div className="setting-row">
        <span>
          <strong>{t("authority.title")}</strong>
          <small>{t("authority.description")}</small>
          {status?.local.daemon_running && (
            <small>{t("authority.stopFirst")}</small>
          )}
        </span>
        <div className="row-actions">
          {manager && (
            <button disabled={busy} onClick={() => setMode("backup")}>
              {t("authority.backup")}
            </button>
          )}
          {manager && (
            <button disabled={busy} onClick={() => setMode("migrate")}>
              {t("authority.migrate")}
            </button>
          )}
          <button
            disabled={busy || !!status?.local.daemon_running}
            onClick={() => setMode("restore")}
          >
            {t("authority.restore")}
          </button>
        </div>
      </div>
      {mode && (
        <AuthorityDialog
          mode={mode}
          status={status}
          close={() => setMode(null)}
        />
      )}
    </section>
  );
}
function AuthorityDialog({
  mode,
  status,
  close,
}: {
  mode: AuthorityAction;
  status: Status | null;
  close: () => void;
}) {
  const { busy, operate, notify } = useOperations();
  const dialog = useRef<HTMLDialogElement>(null);
  const [path, setPath] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [recipient, setRecipient] = useState("");
  const [devices, setDevices] = useState<Device[]>([]);
  const [failure, setFailure] = useState<string | null>(null);
  useEffect(() => {
    dialog.current?.showModal();
  }, []);
  useEffect(() => {
    if (mode !== "migrate") return;
    let active = true;
    void api
      .devices()
      .then(({ devices: items, server_error }) => {
        if (server_error) throw server_error;
        if (active)
          setDevices(
            (items || []).filter(
              (device) =>
                !device.revoked && device.device_id !== status?.local.device_id,
            ),
          );
      })
      .catch((error) => {
        if (active) setFailure(errorText(error));
      });
    return () => {
      active = false;
    };
  }, [mode, status?.local.device_id]);
  const exporting = mode !== "restore";
  return (
    <dialog
      ref={dialog}
      className="authority-dialog"
      onCancel={(event) => {
        event.preventDefault();
        if (!busy) close();
      }}
    >
      <div className="dialog-heading">
        <h2>{t(`authority.${mode}`)}</h2>
        <button
          className="icon-button"
          disabled={busy}
          aria-label={t("common.close")}
          title={t("common.close")}
          onClick={close}
        >
          <Icon name="close" />
        </button>
      </div>
      {failure && (
        <ErrorNotice title={t("authority.failed")} detail={failure} />
      )}
      <form
        onSubmit={async (event) => {
          event.preventDefault();
          if (!path || !password || (exporting && password !== confirmation))
            return;
          setFailure(null);
          const result = await operate(
            async () => {
              try {
                return exporting
                  ? await api.exportAuthority(
                      path,
                      password,
                      mode === "migrate" ? recipient : null,
                    )
                  : await api.restoreAuthority(path, password);
              } catch (error) {
                setFailure(errorText(error));
                throw error;
              }
            },
            {
              name: exporting ? "export_authority" : "restore_authority",
              title: "authority.failed",
            },
          );
          if (result) {
            setPassword("");
            setConfirmation("");
            notify(
              mode === "restore" ? "authority.restored" : "authority.saved",
            );
            close();
          }
        }}
      >
        {mode === "migrate" && (
          <>
            <label htmlFor="authority-recipient">
              {t("authority.recipient")}
            </label>
            <Select
              id="authority-recipient"
              value={recipient}
              onChange={setRecipient}
              disabled={busy}
              required
              options={[
                { value: "", label: t("authority.chooseRecipient") },
                ...devices.map((device) => ({
                  value: device.device_id,
                  label: device.name,
                })),
              ]}
            />
            <p className="field-help">{t("authority.migrationHint")}</p>
          </>
        )}
        <label htmlFor="authority-file">{t("authority.file")}</label>
        <div className="hero-actions">
          <input id="authority-file" value={path} readOnly required />
          <button
            type="button"
            disabled={busy}
            onClick={async () => {
              try {
                const selected = await api.chooseAuthorityArchive(exporting);
                if (selected) setPath(selected);
              } catch (error) {
                setFailure(errorText(error));
              }
            }}
          >
            {t("authority.chooseFile")}
          </button>
        </div>
        <label htmlFor="authority-password">{t("authority.password")}</label>
        <input
          id="authority-password"
          type="password"
          autoComplete={exporting ? "new-password" : "current-password"}
          value={password}
          onChange={(event) => setPassword(event.target.value)}
          minLength={12}
          maxLength={1024}
          required
          disabled={busy}
        />
        <p className="field-help">{t("authority.passwordHint")}</p>
        {exporting && (
          <>
            <label htmlFor="authority-confirmation">
              {t("authority.confirmPassword")}
            </label>
            <input
              id="authority-confirmation"
              type="password"
              autoComplete="new-password"
              value={confirmation}
              onChange={(event) => setConfirmation(event.target.value)}
              required
              disabled={busy}
            />
          </>
        )}
        <div className="hero-actions">
          <button
            className="primary"
            type="submit"
            disabled={
              busy ||
              !path ||
              password.length < 12 ||
              (exporting && password !== confirmation) ||
              (mode === "migrate" && !recipient)
            }
          >
            {t(`authority.${mode}`)}
          </button>
        </div>
      </form>
    </dialog>
  );
}
