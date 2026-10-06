import type { Revocation } from "../../api";
import { useOperations } from "../../app/useOperations";
interface Props {
  result: Revocation;
  manager: boolean;
  deviceName: (id: string) => string;
  dismiss: () => void;
  retry: () => Promise<void>;
}
export function RevocationResult({
  result,
  manager,
  deviceName,
  dismiss,
  retry,
}: Props) {
  const { busy, pending } = useOperations();
  return (
    <section className="panel padded revocation-result" role="status">
      <div className="result-heading">
        <h2>已撤销 {deviceName(result.device_id)}</h2>
        <button
          className="text-button"
          onClick={dismiss}
          aria-label="关闭撤销结果"
        >
          关闭
        </button>
      </div>
      <p>撤销记录已在本机保存。</p>
      {result.sync_error && (
        <p className="warning-text">成员名单同步失败：{result.sync_error}</p>
      )}
      {result.undelivered.length > 0 ? (
        <>
          <p>以下设备尚未确认收到这次更新：</p>
          <ul>
            {result.undelivered.map((id) => (
              <li key={id}>
                {deviceName(id)} <code>{id}</code>
              </li>
            ))}
          </ul>
          <p>
            这些设备收到更新前，可能仍接受被撤销成员。紧急阻断可以在对应设备上暂停远程访问。
          </p>
        </>
      ) : !result.sync_error ? (
        <p>当前其他成员均已确认收到更新。</p>
      ) : null}
      {(result.sync_error || result.undelivered.length > 0) && manager && (
        <button disabled={busy} onClick={() => void retry()}>
          {pending === "revoke" ? "正在同步…" : "重新同步撤销记录"}
        </button>
      )}
    </section>
  );
}
