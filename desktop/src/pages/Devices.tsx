import { useEffect, useState, type ReactNode } from "react";
import type { Device, Status } from "../api";
import { useOperations } from "../app/useOperations";
import type { useDevices } from "../app/useDevices";
import { Icon } from "../components/Icon";
import { t } from "../i18n";
import { ErrorNotice } from "../components/ErrorNotice";
import { DeviceCard } from "./devices/DeviceCard";
import { InvitationDialog } from "./devices/InvitationDialog";
import { RevocationResult } from "./devices/RevocationResult";
import { useMembership } from "./devices/useMembership";
interface Props {
  active: boolean;
  status: Status | null;
  members: ReturnType<typeof useDevices>;
  feedback: ReactNode;
}
export function Devices({ active, status, members, feedback }: Props) {
  const { busy, confirm, action, notify } = useOperations();
  const { loading, error: listError, message, refresh } = members;
  const devices = members.devices.filter(
    (device) => device.device_id !== status?.local.device_id,
  );
  const network = status?.network;
  const manager = !!network?.is_manager;
  const [inviteOpen, setInviteOpen] = useState(false);
  const [openDeviceId, setOpenDeviceId] = useState<string | null>(null);
  const {
    revocation,
    revoke,
    dismiss: dismissRevocation,
  } = useMembership(members);
  useEffect(() => {
    setInviteOpen(false);
    setOpenDeviceId(null);
  }, [network?.network_id]);
  useEffect(() => {
    if (!active) setOpenDeviceId(null);
    if (!active || !manager) setInviteOpen(false);
  }, [active, manager]);
  const deviceName = (id: string) =>
    devices.find((device) => device.device_id === id)?.name || id;
  const revokeMember = async (device: Device) => {
    if (
      await confirm(
        t("revoke.title", { name: device.name }),
        t("revoke.message"),
        { label: t("revoke.confirm"), tone: "danger" },
      )
    )
      await revoke(device.device_id);
  };
  const renderDevice = (device: Device) => (
    <DeviceCard
      key={device.device_id}
      device={device}
      status={status}
      open={openDeviceId === device.device_id}
      toggle={() =>
        setOpenDeviceId((open) =>
          open === device.device_id ? null : device.device_id,
        )
      }
      close={() => setOpenDeviceId(null)}
      revoke={revokeMember}
    />
  );
  const currentMembers = devices.filter((device) => !device.revoked);
  const revoked = devices.filter((device) => device.revoked);
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>{t("devices.title")}</h1>
          <p>
            {manager
              ? t("devices.managerDescription")
              : t("devices.memberDescription")}
          </p>
        </div>
        <div className="heading-actions">
          <button
            disabled={busy || loading || !status?.local.joined}
            onClick={() => void refresh()}
          >
            <Icon name="refresh" />
            {loading ? t("common.refreshing") : t("common.refresh")}
          </button>
          {manager && (
            <button
              className="primary"
              disabled={busy}
              onClick={() => setInviteOpen(true)}
            >
              {t("invite.title")}
            </button>
          )}
        </div>
      </div>
      {!inviteOpen && feedback}
      {listError && (
        <ErrorNotice
          title={
            devices.length
              ? t("devices.refreshFailed")
              : t("devices.listFailed")
          }
          detail={listError}
          retry={() => void refresh()}
        />
      )}
      {status?.local.remote_access_paused && (
        <div className="notice">
          <Icon name="shield" />
          <p>{t("devices.pausedHint")}</p>
        </div>
      )}
      <div className="permission-note">
        <Icon name="shield" />
        <div>
          <strong>{t("devices.direction")}</strong>
          <p>{t("devices.permissionHint")}</p>
        </div>
      </div>
      <section
        className="panel device-list"
        aria-label={t("devices.networkDevices")}
      >
        <div className="list-heading">
          <span>{t("devices.members", { count: currentMembers.length })}</span>
          <span>{t("devices.access")}</span>
          <span />
        </div>
        <div id="devices">{currentMembers.map(renderDevice)}</div>
        {!currentMembers.length && (
          <div className="empty-state">
            {loading ? t("devices.reading") : message}
          </div>
        )}
      </section>
      {revocation && (
        <RevocationResult
          result={revocation}
          manager={manager}
          deviceName={deviceName}
          dismiss={dismissRevocation}
          retry={() => revoke(revocation.device_id)}
        />
      )}
      <details className="panel access-strategy">
        <summary>
          {t("devices.advanced")}
          {status?.local.allow_all && (
            <span className="role-label">{t("devices.allowAllEnabled")}</span>
          )}
        </summary>
        <div className="setting-row">
          <div>
            <strong>{t("devices.allowAll")}</strong>
            <p className="field-help">{t("devices.allowAllHint")}</p>
          </div>
          <label className="switch">
            <input
              type="checkbox"
              aria-label={t("devices.allowAll")}
              disabled={busy || !status?.local.joined}
              checked={!!status?.local.allow_all}
              onChange={async (event) => {
                const allow = event.target.checked;
                if (
                  allow &&
                  !(await confirm(
                    t("devices.allowAllTitle"),
                    t("devices.allowAllMessage"),
                    { label: t("devices.allowAllConfirm") },
                  ))
                )
                  return;
                if (
                  await action({ command: "all_permissions", args: { allow } })
                )
                  notify(allow ? "devices.allowAllOn" : "devices.allowAllOff");
              }}
            />
            <span className="switch-track" />
          </label>
        </div>
      </details>
      {revoked.length > 0 && (
        <details className="panel revoked-devices">
          <summary>
            {t("devices.revokedMembers", { count: revoked.length })}
          </summary>
          {revoked.map(renderDevice)}
        </details>
      )}
      <p className="footnote">{t("devices.footnote")}</p>
      {manager && (
        <InvitationDialog
          open={inviteOpen}
          close={() => setInviteOpen(false)}
          active={active}
          status={status}
          feedback={feedback}
        />
      )}
    </>
  );
}
