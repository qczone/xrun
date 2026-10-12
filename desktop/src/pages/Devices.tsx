import { useEffect, useState, type ReactNode } from "react";
import type { Device, Status } from "../api";
import { useOperations } from "../app/useOperations";
import type { useDevices } from "../app/useDevices";
import { Icon } from "../components/Icon";
import { compactIdentifier } from "../format";
import { t } from "../i18n";
import { ErrorNotice } from "../components/ErrorNotice";
import { NetworkSetup } from "../components/NetworkSetup";
import { TrafficSummary } from "../components/TrafficSummary";
import { RefreshButton } from "../components/RefreshButton";
import { DeviceRow } from "./devices/DeviceRow";
import { InvitationDialog } from "./devices/InvitationDialog";
import { RevocationResult } from "./devices/RevocationResult";
import { useMembership } from "./devices/useMembership";
import { RemoteCommandDialog } from "./devices/RemoteCommandDialog";
import { FileDialog } from "./devices/FileDialog";
import { ScreenshotDialog } from "./devices/ScreenshotDialog";

interface Props {
  active: boolean;
  paused: boolean;
  status: Status | null;
  members: ReturnType<typeof useDevices>;
  feedback: ReactNode;
}
export function Devices({ active, paused, status, members, feedback }: Props) {
  const { busy, pending, confirm, action, clearError } = useOperations();
  const { loading, refreshing, error: listError, message, refresh } = members;
  const devices = members.devices.filter(
    (device) => device.device_id !== status?.local.device_id,
  );
  const network = status?.network;
  const joined = !!status?.local.joined;
  const manager = !!network?.is_manager;
  const [inviteOpen, setInviteOpen] = useState(false);
  const [commandDevice, setCommandDevice] = useState<Device | null>(null);
  const [fileDevice, setFileDevice] = useState<Device | null>(null);
  const [captureDevice, setCaptureDevice] = useState<Device | null>(null);
  const [openDeviceId, setOpenDeviceId] = useState<string | null>(null);
  const [deviceFilter, setDeviceFilter] = useState<"all" | "online">("all");
  const [search, setSearch] = useState("");
  const {
    revocation,
    revoke,
    dismiss: dismissRevocation,
  } = useMembership(members);
  useEffect(() => {
    setInviteOpen(false);
    setCommandDevice(null);
    setCaptureDevice(null);
    setFileDevice(null);
    setOpenDeviceId(null);
    setDeviceFilter("all");
    setSearch("");
  }, [network?.network_id]);
  useEffect(() => {
    if (!active) {
      setOpenDeviceId(null);
      setCommandDevice(null);
      setCaptureDevice(null);
      setFileDevice(null);
    }
    if (!active || !manager) setInviteOpen(false);
  }, [active, manager]);
  const revokeMember = async (device: Device) => {
    if (
      await confirm(
        t("revoke.title", { name: device.name }),
        t("revoke.message"),
        {
          label: t("revoke.confirm"),
          tone: "danger",
        },
      )
    )
      await revoke(device.device_id);
  };
  const renderDevice = (device: Device) => (
    <DeviceRow
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
      runCommand={(device) => {
        clearError();
        setCommandDevice(device);
      }}
      transferFile={(device) => {
        clearError();
        setFileDevice(device);
      }}
      captureScreen={(device) => {
        clearError();
        setCaptureDevice(device);
      }}
    />
  );
  const currentMembers = devices.filter((device) => !device.revoked);
  const revokedMembers = devices.filter((device) => device.revoked);
  const onlineCount = currentMembers.filter((device) => device.online).length;
  const matchingMembers = currentMembers.filter(
    (device) =>
      (deviceFilter === "all" || device.online) &&
      `${device.name} ${device.os || ""} ${device.device_id} ${compactIdentifier(device.device_id)}`
        .toLocaleLowerCase()
        .includes(search.toLocaleLowerCase()),
  );
  return (
    <>
      <div className="page-heading">
        <div>
          <h1>{t(joined ? "devices.title" : "setup.connectTitle")}</h1>
          <p>
            {t(joined ? "devices.description" : "setup.connectDescription")}
          </p>
        </div>
        {joined && manager && (
          <button
            className="primary"
            disabled={busy}
            onClick={() => setInviteOpen(true)}
          >
            <Icon name="plus" />
            {t("invite.title")}
          </button>
        )}
      </div>
      {!inviteOpen && !commandDevice && feedback}
      <NetworkSetup
        joined={joined}
        busy={busy}
        pending={pending}
        startFailed={
          !!status?.error && status.error.code === "SERVICE_START_FAILED"
        }
        action={action}
        onCreated={() => {}}
      />
      {joined && (
        <>
          {listError && (
            <ErrorNotice
              title={t(
                devices.length ? "devices.refreshFailed" : "devices.listFailed",
              )}
              detail={listError}
              retry={() => void refresh()}
            />
          )}
          <div className="list-toolbar">
            <div className="filter-tabs" aria-label={t("devices.filter")}>
              {(["all", "online"] as const).map((filter) => (
                <button
                  key={filter}
                  className={`filter-tab ${deviceFilter === filter ? "selected" : ""}`}
                  aria-pressed={deviceFilter === filter}
                  onClick={() => setDeviceFilter(filter)}
                >
                  {t(filter === "all" ? "devices.all" : "devices.online")}
                  <span>
                    {filter === "all" ? currentMembers.length : onlineCount}
                  </span>
                </button>
              ))}
            </div>
            <div className="list-toolbar-actions">
              <label className="list-search">
                <Icon name="search" />
                <input
                  type="text"
                  aria-label={t("devices.search")}
                  placeholder={t("devices.search")}
                  value={search}
                  onChange={(event) => setSearch(event.target.value)}
                />
              </label>
              <RefreshButton
                compact
                refreshing={refreshing}
                disabled={busy}
                onClick={() => void refresh()}
              />
            </div>
          </div>
          <section
            className="device-list"
            aria-label={t("devices.networkDevices")}
            aria-busy={refreshing}
          >
            <div className="list-heading">
              <span>{t("devices.name")}</span>
              <span>{t("devices.state")}</span>
              <span>{t("devices.access")}</span>
              <span />
            </div>
            <div id="devices">{matchingMembers.map(renderDevice)}</div>
            {!matchingMembers.length && (
              <div className="empty-state">
                {loading
                  ? t("devices.reading")
                  : currentMembers.length
                    ? t("devices.noMatch")
                    : message}
              </div>
            )}
          </section>
          <p className="permission-hint">
            <Icon name="shield" />
            {t(
              manager
                ? "devices.managerProtected"
                : status?.local.remote_access_paused
                  ? "devices.pausedHint"
                  : status?.local.allow_all
                    ? "devices.allowAllSummary"
                    : "devices.permissionSummary",
            )}
          </p>
          {revocation && (
            <RevocationResult
              result={revocation}
              manager={manager}
              deviceName={(id) =>
                devices.find((device) => device.device_id === id)?.name ||
                compactIdentifier(id)
              }
              dismiss={dismissRevocation}
              retry={() => revoke(revocation.device_id)}
            />
          )}
          {revokedMembers.length > 0 && (
            <details className="revoked-devices">
              <summary>
                {t("devices.revokedMembers", { count: revokedMembers.length })}
              </summary>
              {revokedMembers.map(renderDevice)}
            </details>
          )}
          <TrafficSummary
            status={status}
            active={active && !paused && !inviteOpen}
            devices={devices}
          />
          {!manager && (
            <p className="footnote">{t("devices.memberDescription")}</p>
          )}
        </>
      )}
      {manager && (
        <InvitationDialog
          open={inviteOpen}
          close={() => setInviteOpen(false)}
          active={active}
          status={status}
          feedback={feedback}
        />
      )}
      {commandDevice && (
        <RemoteCommandDialog
          key={commandDevice.device_id}
          device={
            devices.find(
              (device) => device.device_id === commandDevice.device_id,
            ) || commandDevice
          }
          active={active && !paused}
          close={() => setCommandDevice(null)}
          feedback={feedback}
        />
      )}
      {fileDevice && (
        <FileDialog
          device={
            members.devices.find(
              (device) => device.device_id === fileDevice.device_id,
            ) || fileDevice
          }
          close={() => setFileDevice(null)}
        />
      )}
      {captureDevice && (
        <ScreenshotDialog
          device={
            devices.find(
              (device) => device.device_id === captureDevice.device_id,
            ) || captureDevice
          }
          close={() => setCaptureDevice(null)}
        />
      )}
    </>
  );
}
