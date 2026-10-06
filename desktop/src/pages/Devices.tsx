import { useEffect, useRef, useState, type ReactNode } from "react";
import type {
  Action,
  Confirm,
  Device,
  Operation,
  PendingOperation,
  Revocation,
  Status,
} from "../api";
import { Icon } from "../components/Icon";
import { InvitePanel } from "../components/InvitePanel";
import { ErrorNotice } from "../components/ErrorNotice";
import { osName } from "../format";

interface Props {
  active: boolean;
  status: Status | null;
  devices: Device[];
  busy: boolean;
  pending: PendingOperation | null;
  feedback: ReactNode;
  loading: boolean;
  listError: string | null;
  message: string;
  refresh: () => Promise<void>;
  action: Action;
  confirm: Confirm;
  notify: (message: string) => void;
  operate: Operation;
  revoke: (device: string) => Promise<Revocation | undefined>;
}

export function Devices({
  active,
  status,
  devices,
  busy,
  pending,
  feedback,
  loading,
  listError,
  message,
  refresh,
  action,
  confirm,
  notify,
  operate,
  revoke,
}: Props) {
  const [revocation, setRevocation] = useState<Revocation | null>(null);
  const [inviteOpen, setInviteOpen] = useState(false);
  const inviteDialog = useRef<HTMLDialogElement>(null);
  const network = status?.network;
  const manager = !!network?.is_manager;
  useEffect(() => {
    setRevocation(null);
    setInviteOpen(false);
  }, [network?.network_id]);
  useEffect(() => {
    if (!active || !manager) setInviteOpen(false);
  }, [active, manager]);
  useEffect(() => {
    const closeMenus = (event?: PointerEvent) => {
      document
        .querySelectorAll<HTMLDetailsElement>(
          "#page-devices .device-more[open]",
        )
        .forEach((menu) => {
          if (!event || !menu.contains(event.target as Node)) menu.open = false;
        });
    };
    if (!active) {
      closeMenus();
      return;
    }
    document.addEventListener("pointerdown", closeMenus);
    return () => document.removeEventListener("pointerdown", closeMenus);
  }, [active]);
  useEffect(() => {
    if (inviteOpen && inviteDialog.current && !inviteDialog.current.open)
      inviteDialog.current.showModal();
    else if (!inviteOpen && inviteDialog.current?.open)
      inviteDialog.current.close();
  }, [inviteOpen]);
  const deviceName = (id: string) =>
    devices.find((d) => d.device_id === id)?.name || id;
  const revokeMember = async (device: Device) => {
    if (
      !(await confirm(
        `撤销 ${device.name} 的成员身份？`,
        "收到撤销记录的设备将拒绝该成员。离线设备需要等收到更新后才生效；已经受理的后台任务不会自动取消。重新加入需要新的身份和邀请。",
        { label: "撤销成员身份", tone: "danger" },
      ))
    )
      return;
    const result = await revoke(device.device_id);
    if (result) setRevocation(result);
  };
  const renderDevice = (device: Device) => {
    const denied = !!status?.deny_from.includes(device.device_id);
    const individual = !!status?.allow_from.includes(device.device_id);
    const allowed =
      !device.revoked && !denied && (!!status?.local.allow_all || individual);
    const source = device.revoked
      ? "成员已撤销"
      : denied
        ? "单独拒绝"
        : individual
          ? "单独授权"
          : status?.local.allow_all
            ? "来自全体授权"
            : "未授权";
    return (
      <div className="device" key={device.device_id}>
        <div className="device-identity">
          <div className="device-icon">
            <Icon name="monitor" />
          </div>
          <div className="device-details">
            <div className="device-name">
              {device.name}
              {device.admin && <span className="role-label">管理设备</span>}
            </div>
            <div className="device-info">
              <span
                className={`dot ${device.online && !device.revoked ? "online" : ""}`}
              />
              <span>
                {device.revoked
                  ? "已撤销"
                  : device.online
                    ? "中转报告已连接"
                    : "未连接中转"}
                {device.os && <> · {osName(device.os)}</>}
              </span>
            </div>
          </div>
        </div>
        <div className="device-permission">
          <label className="switch">
            <input
              type="checkbox"
              disabled={busy || device.revoked || !status?.local.joined}
              aria-label={`允许 ${device.name} 访问本机`}
              checked={allowed}
              onChange={async (event) => {
                const allow = event.target.checked;
                if (
                  allow &&
                  !(await confirm(
                    `允许 ${device.name} 访问本机？`,
                    "该设备将能以你的用户权限执行命令、传输文件和截图。请只授权你信任的设备。",
                    { label: "允许访问本机" },
                  ))
                )
                  return;
                if (
                  await action({
                    command: "permission",
                    args: { device: device.device_id, allow },
                  })
                )
                  notify(allow ? "已允许访问本机" : "已拒绝访问本机");
              }}
            />
            <span className="switch-track" />
          </label>
          <small>{source}</small>
        </div>
        <details
          className="device-more"
          name="device-actions"
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              event.currentTarget.open = false;
              event.currentTarget.querySelector("summary")?.focus();
            }
          }}
        >
          <summary aria-label={`${device.name} 的更多操作`}>更多</summary>
          <div className="device-more-body">
            <span className="field-help">设备 ID</span>
            <code>{device.device_id}</code>
            {manager &&
              !device.revoked &&
              device.device_id !== network?.manager_id && (
                <button
                  className="danger-button"
                  disabled={busy}
                  aria-label={`撤销 ${device.name} 的成员身份`}
                  onClick={(event) => {
                    event.currentTarget.closest("details")!.open = false;
                    void revokeMember(device);
                  }}
                >
                  撤销成员身份…
                </button>
              )}
          </div>
        </details>
      </div>
    );
  };
  const members = devices.filter((device) => !device.revoked);
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
          title="设备列表未能刷新，已保留上次读取的结果。"
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
          <span>网络成员 · {members.length}</span>
          <span>访问本机</span>
          <span />
        </div>
        <div id="devices">{members.map(renderDevice)}</div>
        {!members.length && (
          <div className="empty-state">
            {loading ? "正在读取设备列表…" : message}
          </div>
        )}
      </section>
      {revocation && (
        <section className="panel padded revocation-result" role="status">
          <div className="result-heading">
            <h2>已撤销 {deviceName(revocation.device_id)}</h2>
            <button
              className="text-button"
              onClick={() => setRevocation(null)}
              aria-label="关闭撤销结果"
            >
              关闭
            </button>
          </div>
          <p>撤销记录已在本机保存。</p>
          {revocation.sync_error && (
            <p className="warning-text">
              成员名单同步失败：{revocation.sync_error}
            </p>
          )}
          {revocation.undelivered.length > 0 ? (
            <>
              <p>以下设备尚未确认收到这次更新：</p>
              <ul>
                {revocation.undelivered.map((id) => (
                  <li key={id}>
                    {deviceName(id)} <code>{id}</code>
                  </li>
                ))}
              </ul>
              <p>
                这些设备收到更新前，可能仍接受被撤销成员。紧急阻断可以在对应设备上暂停远程访问。
              </p>
            </>
          ) : !revocation.sync_error ? (
            <p>当前其他成员均已确认收到更新。</p>
          ) : null}
          {(revocation.sync_error || revocation.undelivered.length > 0) &&
            manager && (
              <button
                disabled={busy}
                onClick={async () => {
                  const result = await revoke(revocation.device_id);
                  if (result) setRevocation(result);
                }}
              >
                {pending === "revoke" ? "正在同步…" : "重新同步撤销记录"}
              </button>
            )}
        </section>
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
        <dialog
          ref={inviteDialog}
          id="invite-dialog"
          className="invite-dialog"
          aria-labelledby="invite-title"
          onClose={() => setInviteOpen(false)}
        >
          <div className="dialog-heading">
            <h2 id="invite-title">邀请新设备</h2>
            <button
              className="text-button"
              aria-label="关闭邀请"
              onClick={() => setInviteOpen(false)}
            >
              关闭
            </button>
          </div>
          {inviteOpen && (
            <>
              <div>{feedback}</div>
              <InvitePanel
                active={active && inviteOpen}
                status={status}
                busy={busy}
                pending={pending}
                operate={operate}
                confirm={confirm}
                notify={notify}
              />
            </>
          )}
        </dialog>
      )}
    </>
  );
}
