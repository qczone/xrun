import { useEffect, useRef, useState } from "react";
import {
  api,
  type Confirm,
  type Invitation,
  type Operation,
  type Status,
} from "../api";

interface Props {
  active: boolean;
  status: Status | null;
  busy: boolean;
  operate: Operation;
  confirm: Confirm;
  notify: (message: string) => void;
}

export function InvitePanel({
  active,
  status,
  busy,
  operate,
  confirm,
  notify,
}: Props) {
  const [allow, setAllow] = useState(false);
  const [invitation, setInvitation] = useState<
    (Invitation & { expiresAt: number }) | null
  >(null);
  const [expired, setExpired] = useState(false);
  const generation = useRef(0);
  const network = status?.network;
  const manager = !!network?.is_manager;
  useEffect(() => {
    generation.current++;
    setInvitation(null);
    setAllow(false);
    setExpired(false);
  }, [active, manager, network?.network_id]);
  useEffect(() => {
    if (!invitation) return;
    const timer = setTimeout(
      () => {
        setInvitation(null);
        setExpired(true);
      },
      Math.max(0, invitation.expiresAt - Date.now()),
    );
    return () => clearTimeout(timer);
  }, [invitation]);
  if (!manager) return null;

  const generate = async () => {
    const request = generation.current;
    const grant = allow;
    if (
      grant &&
      !(await confirm(
        "邀请新设备并互相授权？",
        "新设备将能以你的用户权限访问本机，本机也能访问新设备。请只向你信任的设备分享链接。",
      ))
    )
      return;
    if (request !== generation.current) return;
    setInvitation(null);
    setExpired(false);
    const result = await operate(() => api.invite(grant));
    if (result && request === generation.current)
      setInvitation({
        ...result,
        expiresAt: Date.now() + result.expires_in * 1000,
      });
  };
  return (
    <section className="panel padded" aria-label="邀请新设备">
      <div className="section-title">
        <h2>邀请新设备</h2>
        <p>
          由本机生成邀请。链接单次使用，10
          分钟内有效；加入时本机后台服务需要在线。
        </p>
      </div>
      <label className="invitation-option">
        <input
          type="checkbox"
          checked={allow}
          disabled={busy}
          onChange={(event) => {
            setAllow(event.target.checked);
            setInvitation(null);
          }}
        />
        允许新设备与本机互相访问
      </label>
      <p className="field-help">
        默认仅加入网络，不授予命令执行或文件访问权限。
      </p>
      <button
        className="primary"
        disabled={busy || !status?.local.daemon_connected}
        onClick={() => void generate()}
      >
        生成邀请链接
      </button>
      {!status?.local.daemon_connected && (
        <p className="field-help">先启动后台服务并连接中转，再生成邀请。</p>
      )}
      {expired && <p role="status">链接已过期，请重新生成。</p>}
      {invitation && (
        <div className="invitation-result">
          <label htmlFor="invitation-link">生成的邀请链接</label>
          <textarea
            id="invitation-link"
            className="mono"
            value={invitation.link}
            readOnly
            rows={3}
            spellCheck={false}
          />
          <p className="field-help">
            {invitation.allow
              ? "加入后双方互相授权。"
              : "仅注册设备，加入后需要分别授权。"}{" "}
            请在 {new Date(invitation.expiresAt).toLocaleTimeString()} 前使用。
          </p>
          <div className="hero-actions">
            <button
              disabled={busy}
              onClick={async () => {
                if (Date.now() >= invitation.expiresAt) {
                  setInvitation(null);
                  setExpired(true);
                  return;
                }
                const copied = await operate(async () => {
                  await api.copyInvitation(invitation.link);
                  return true;
                });
                if (copied) notify("邀请链接已复制");
              }}
            >
              复制链接
            </button>
            <button disabled={busy} onClick={() => setInvitation(null)}>
              隐藏链接
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
