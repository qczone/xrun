import { useState } from "react";
import type { Action } from "../api";
import { Icon } from "./Icon";

interface Props {
  joined: boolean;
  busy: boolean;
  action: Action;
  onCreated: () => void;
}

export function NetworkSetup({ joined, busy, action, onCreated }: Props) {
  const [mode, setMode] = useState<"join" | "create">("join");
  const [link, setLink] = useState("");
  const [name, setName] = useState("");
  const [retry, setRetry] = useState(false);
  const creating = mode === "create";
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
              先在 Linux 中转主机运行 <code>xrun relay install</code> 或{" "}
              <code>xrun relay invite</code>，再粘贴输出的部署链接。
              创建后，本机成为管理设备，负责邀请和撤销成员。
            </>
          ) : (
            "向管理设备获取邀请链接，再将这台设备加入网络。"
          )}
        </p>
      </div>
      <form
        id={creating ? "create-network-form" : "join-form"}
        onSubmit={async (event) => {
          event.preventDefault();
          const success = await action({
            command: creating ? "create_network" : "join",
            args: { link, name },
          });
          if (success) {
            setLink("");
            setRetry(false);
            if (creating) onCreated();
          } else if (creating) {
            // Creation may save the identity before publishing or starting the service.
            setRetry(true);
          }
        }}
      >
        <label htmlFor="setup-link">
          {creating ? "中转部署链接" : "邀请链接"}
        </label>
        <input
          id="setup-link"
          type="password"
          placeholder={creating ? "xrun-relay://…" : "xrun://…"}
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
          placeholder="mac1 或 win1"
          pattern="[a-z][a-z0-9-]{0,31}"
          maxLength={32}
          autoComplete="off"
          spellCheck={false}
          required
          disabled={busy}
          value={name}
          onChange={(event) => setName(event.target.value)}
        />
        <p className="field-help">小写字母、数字和短横线，以字母开头。</p>
        <div className="hero-actions">
          <button className="primary" type="submit" disabled={busy}>
            {creating
              ? retry
                ? "重试创建并启动"
                : "创建并启动服务"
              : "加入并启动服务"}
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
