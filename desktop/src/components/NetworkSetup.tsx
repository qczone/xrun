import { useEffect, useRef, useState } from "react";
import type { Action, PendingOperation } from "../api";
import { Icon } from "./Icon";

interface Props {
  joined: boolean;
  busy: boolean;
  pending: PendingOperation | null;
  startFailed: boolean;
  action: Action;
  onCreated: () => void;
}

export function NetworkSetup({
  joined,
  busy,
  pending,
  startFailed,
  action,
  onCreated,
}: Props) {
  const [mode, setMode] = useState<"join" | "create">("join");
  const [link, setLink] = useState("");
  const [name, setName] = useState("");
  const [retry, setRetry] = useState(false);
  const submitting = useRef(false);
  const creating = mode === "create";
  useEffect(() => {
    if (joined && startFailed) {
      setLink("");
      setRetry(false);
    }
  }, [joined, retry, startFailed]);
  if (joined && !retry) return null;

  return (
    <section className="panel padded">
      {!joined && (
        <div className="setup-tabs" aria-label="连接方式">
          {(["join", "create"] as const).map((value) => (
            <button
              key={value}
              aria-pressed={mode === value}
              className={mode === value ? "selected" : ""}
              disabled={busy}
              onClick={() => {
                setMode(value);
                setLink("");
                setRetry(false);
              }}
            >
              {value === "create" ? "创建新网络" : "加入已有网络"}
            </button>
          ))}
        </div>
      )}
      <div className="section-title">
        <h2>{creating ? "在本机创建网络" : "加入已有网络"}</h2>
        <p>
          {creating ? (
            <>
              粘贴中转部署完成后提供的 HTTPS 地址或部署链接。
              创建后，本机成为管理设备，负责邀请和撤销成员。
            </>
          ) : (
            "向管理设备获取邀请链接，再将这台设备加入网络。"
          )}
        </p>
      </div>
      <form
        className="network-setup-form"
        id={creating ? "create-network-form" : "join-form"}
        onSubmit={async (event) => {
          event.preventDefault();
          if (busy || submitting.current) return;
          submitting.current = true;
          const success = await action({
            command: creating ? "create_network" : "join",
            args: { link, name },
          });
          if (success) {
            setLink("");
            setRetry(false);
            if (creating) onCreated();
          } else if (creating) {
            // Creation may save the identity before starting the service.
            setRetry(true);
          }
          submitting.current = false;
        }}
      >
        <label htmlFor="setup-link">
          {creating ? "中转部署链接" : "邀请链接"}
        </label>
        <input
          id="setup-link"
          type="password"
          placeholder={creating ? "https://… 或 xrun-relay://…" : "xrun://…"}
          autoComplete="off"
          spellCheck={false}
          required
          disabled={busy}
          value={link}
          onChange={(event) => setLink(event.target.value)}
        />
        <label htmlFor="setup-name">本机名称</label>
        <input
          id="setup-name"
          type="text"
          placeholder="例如 mac1 或 win1"
          pattern="[a-z][a-z0-9-]{0,31}"
          maxLength={32}
          autoComplete="off"
          spellCheck={false}
          required
          disabled={busy}
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
        <p className="field-help">
          1–32 位小写字母、数字和短横线，以字母开头。
        </p>
        <p className="setup-help">完成后自动启动本机后台服务。</p>
        <div className="hero-actions">
          <button className="primary" type="submit" disabled={busy}>
            {pending === "create_network"
              ? "正在创建…"
              : pending === "join"
                ? "正在加入…"
                : creating
                  ? retry
                    ? "重试创建网络"
                    : "创建网络"
                  : "加入网络"}
            <Icon name="arrow" />
          </button>
          {joined && retry && (
            <button
              type="button"
              disabled={busy}
              onClick={() => {
                setLink("");
                setRetry(false);
              }}
            >
              关闭
            </button>
          )}
        </div>
      </form>
    </section>
  );
}
