import { useEffect, useState, type ReactNode } from "react";
import type { Device, Status } from "../api";
import { useOperations } from "../app/useOperations";
import type { useDevices } from "../app/useDevices";
import { Icon } from "../components/Icon";
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
        `撤销 ${device.name} 的成员身份？`,
        "收到撤销记录的设备将拒绝该成员。离线设备需要等收到更新后才生效；已经受理的后台任务不会自动取消。重新加入需要新的身份和邀请。",
        { label: "撤销成员身份", tone: "danger" },
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
          <h1>设备与访问</h1>
          <p>
            {manager
              ? "管理网络成员，以及其他设备对本机的访问权限。"
              : "管理其他设备对本机的访问权限；邀请和撤销由管理设备负责。"}
          </p>
        </div>
        <div className="heading-actions">
          <button
            disabled={busy || loading || !status?.local.joined}
            onClick={() => void refresh()}
          >
            <Icon name="refresh" />
            {loading ? "正在刷新…" : "刷新"}
          </button>
          {manager && (
            <button
              className="primary"
              disabled={busy}
              onClick={() => setInviteOpen(true)}
            >
              邀请新设备
            </button>
          )}
        </div>
      </div>
      {!inviteOpen && feedback}
      {listError && (
        <ErrorNotice
          title={
            devices.length
              ? "设备列表未能刷新，已保留上次读取的结果。"
              : "设备列表暂时不可用，请检查连接后重试。"
          }
          detail={listError}
          retry={() => void refresh()}
        />
      )}
      {status?.local.remote_access_paused && (
        <div className="notice">
          <Icon name="shield" />
          <p>远程访问已暂停，以下授权暂不生效；恢复后沿用这些设置。</p>
        </div>
      )}
      <div className="permission-note">
        <Icon name="shield" />
        <div>
          <strong>其他设备 → 本机</strong>
          <p>
            允许的设备可以执行命令、传输文件和截图。访问其他设备仍需要对方授权。
          </p>
        </div>
      </div>
      <section className="panel device-list" aria-label="网络设备">
        <div className="list-heading">
          <span>网络成员 · {currentMembers.length}</span>
          <span>访问本机</span>
          <span />
        </div>
        <div id="devices">{currentMembers.map(renderDevice)}</div>
        {!currentMembers.length && (
          <div className="empty-state">
            {loading ? "正在读取设备列表…" : message}
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
          高级访问策略
          {status?.local.allow_all && (
            <span className="role-label">全体授权已开启</span>
          )}
        </summary>
        <div className="setting-row">
          <div>
            <strong>允许全体成员访问本机</strong>
            <p className="field-help">
              包括以后通过邀请加入的成员；单独拒绝的设备除外。关闭后保留单独授权。
            </p>
          </div>
          <label className="switch">
            <input
              type="checkbox"
              aria-label="允许全体成员访问本机"
              disabled={busy || !status?.local.joined}
              checked={!!status?.local.allow_all}
              onChange={async (event) => {
                const allow = event.target.checked;
                if (
                  allow &&
                  !(await confirm(
                    "允许全体成员访问本机？",
                    "当前和以后加入的设备都可以用你的用户权限执行命令、传文件和截图。获得邀请的人加入后也会获得访问权；单独拒绝仍然生效。",
                    { label: "允许全体成员" },
                  ))
                )
                  return;
                if (
                  await action({ command: "all_permissions", args: { allow } })
                )
                  notify(
                    allow
                      ? "已允许当前和未来成员访问本机"
                      : "已关闭全体授权，保留单独权限",
                  );
              }}
            />
            <span className="switch-track" />
          </label>
        </div>
      </details>
      {revoked.length > 0 && (
        <details className="panel revoked-devices">
          <summary>已撤销成员 · {revoked.length}</summary>
          {revoked.map(renderDevice)}
        </details>
      )}
      <p className="footnote">
        这里的授权只影响本机。设备连接中转，也需要获得授权才能访问本机。
      </p>
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
