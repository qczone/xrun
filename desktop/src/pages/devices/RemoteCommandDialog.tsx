import {
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  api,
  type CommandJob,
  type Device,
  type RemoteReceipt,
} from "../../api";
import { useOperations } from "../../app/useOperations";
import { ErrorNotice } from "../../components/ErrorNotice";
import { Icon } from "../../components/Icon";
import { Select } from "../../components/Select";
import { errorCode, errorText } from "../../errors";
import { dateTime, isRunning, jobState } from "../../format";
import { t } from "../../i18n";
import { OutputBuffer } from "../../logs";

export function RemoteCommandDialog({
  device,
  active,
  close,
  feedback,
}: {
  device: Device;
  active: boolean;
  close: () => void;
  feedback: ReactNode;
}) {
  const { busy, operate, confirm, clearError } = useOperations();
  const dialog = useRef<HTMLDialogElement>(null);
  const submitting = useRef(false);
  const outputBuffer = useRef(new OutputBuffer());
  const outputLog = useRef<HTMLPreElement>(null);
  const followOutput = useRef(true);
  const [script, setScript] = useState("");
  const [shell, setShell] = useState(
    device.os === "windows" ? "powershell" : "sh",
  );
  const [cwd, setCwd] = useState("");
  const [timeout, setTimeoutSeconds] = useState(1800);
  const [requestId, setRequestId] = useState<string | null>(null);
  const [job, setJob] = useState<CommandJob | null>(null);
  const [receipts, setReceipts] = useState<RemoteReceipt[]>([]);
  const [readError, setReadError] = useState<string | null>(null);
  const [reading, setReading] = useState(false);
  const [revision, setRevision] = useState(0);
  const [outputRevision, setOutputRevision] = useState(0);
  const canRun = device.online && !device.revoked;
  useLayoutEffect(() => {
    if (followOutput.current && outputLog.current)
      outputLog.current.scrollTop = outputLog.current.scrollHeight;
  }, [outputRevision]);
  useEffect(() => {
    if (dialog.current && !dialog.current.open) dialog.current.showModal();
  }, []);
  useEffect(() => {
    let mounted = true;
    void api
      .remoteRecent(device.device_id)
      .then((items) => {
        if (mounted) setReceipts(items || []);
      })
      .catch((error) => {
        if (mounted) setReadError(errorText(error));
      });
    return () => {
      mounted = false;
    };
  }, [device.device_id, revision]);
  const selectRequest = async (id: string) => {
    if (busy || reading) return;
    setRequestId(id);
    setJob(null);
    outputBuffer.current = new OutputBuffer();
    setReading(true);
    setReadError(null);
    clearError();
    try {
      setJob(await api.remoteQuery(device.device_id, id));
    } catch (error) {
      setReadError(errorText(error));
    } finally {
      setReading(false);
    }
  };
  const jobId = job?.job_id;
  const dbId = job?.db_id;
  useEffect(() => {
    if (!active || !jobId || !dbId) return;
    let mounted = true;
    let pending = false;
    let finished = false;
    const load = async () => {
      if (!mounted || pending || finished) return;
      pending = true;
      try {
        const result = await api.remoteOutput(
          device.device_id,
          dbId,
          jobId,
          outputBuffer.current.after || 0,
        );
        if (!mounted) return;
        outputBuffer.current.consume(result);
        setJob(result.job);
        setReadError(null);
        setOutputRevision((value) => value + 1);
        finished = !isRunning(result.job) && !result.has_more;
        if (result.has_more) globalThis.setTimeout(() => void load(), 0);
      } catch (error) {
        if (mounted) setReadError(errorText(error));
        if (
          [
            "DB_RESET",
            "JOB_NOT_FOUND",
            "DEVICE_REVOKED",
            "PERMISSION_DENIED",
            "SOURCE_NOT_ALLOWED",
            "DB_SCHEMA_MISMATCH",
          ].includes(errorCode(error) || "")
        )
          finished = true;
      } finally {
        pending = false;
      }
    };
    void load();
    const timer = globalThis.setInterval(() => void load(), 3000);
    return () => {
      mounted = false;
      globalThis.clearInterval(timer);
    };
  }, [active, device.device_id, jobId, dbId, revision]);
  const submit = async () => {
    if (busy || submitting.current || requestId || !canRun) return;
    submitting.current = true;
    const id = crypto.randomUUID();
    setRequestId(id);
    setReadError(null);
    const result = await operate(
      async () => {
        try {
          return await api.remoteSubmit(device.device_id, {
            request_id: id,
            script,
            shell,
            cwd: cwd.trim() || null,
            timeout,
          });
        } catch (error) {
          if (
            [
              "SOURCE_NOT_ALLOWED",
              "INVALID_REQUEST",
              "DEVICE_OFFLINE",
              "DEVICE_BUSY",
              "DEVICE_REVOKED",
            ].includes(errorCode(error) || "")
          ) {
            setRequestId(null);
          }
          throw error;
        }
      },
      { name: "remote_submit", title: "remote.submitFailed" },
    );
    if (result) setJob(result);
    setRevision((value) => value + 1);
    submitting.current = false;
  };
  const cancel = async () => {
    if (
      !job ||
      !(await confirm(t("remote.cancelTitle"), t("remote.cancelHint"), {
        label: t("remote.cancel"),
        tone: "danger",
      }))
    )
      return;
    const result = await operate(
      () => api.remoteCancel(device.device_id, job.db_id, job.job_id),
      { name: "remote_cancel", title: "remote.cancelFailed" },
    );
    if (result) setJob(result);
    setRevision((value) => value + 1);
  };
  const newCommand = () => {
    setRequestId(null);
    setJob(null);
    setReadError(null);
    outputBuffer.current = new OutputBuffer();
    setOutputRevision((value) => value + 1);
    clearError();
  };
  return (
    <dialog
      ref={dialog}
      className="remote-command-dialog"
      aria-labelledby="remote-command-title"
      onClose={close}
      onCancel={(event) => {
        if (busy) event.preventDefault();
      }}
    >
      <div className="dialog-heading">
        <h2 id="remote-command-title">
          {t("remote.title", { name: device.name })}
        </h2>
        <button
          className="text-button"
          disabled={busy}
          aria-label={t("remote.close")}
          onClick={close}
        >
          {t("common.close")}
        </button>
      </div>
      <p className="field-help">{t("remote.direction")}</p>
      {feedback}
      {readError && (
        <ErrorNotice
          title={t("remote.readFailed")}
          detail={readError}
          retry={() =>
            requestId && !job
              ? void selectRequest(requestId)
              : setRevision((value) => value + 1)
          }
        />
      )}
      {receipts.length > 0 && (
        <div className="remote-recent">
          <label htmlFor="remote-recent">{t("remote.recent")}</label>
          <Select
            id="remote-recent"
            value={
              receipts.some((item) => item.request_id === requestId)
                ? requestId || ""
                : ""
            }
            disabled={busy || reading}
            fullWidth
            onChange={(value) => {
              if (value) void selectRequest(value);
              else newCommand();
            }}
            options={[
              { value: "", label: t("remote.new") },
              ...receipts.map((receipt) => ({
                value: receipt.request_id,
                label: `${dateTime(receipt.created_at_ms)} · ${
                  receipt.job_id ||
                  t(
                    receipt.status === "not_accepted"
                      ? "remote.notAccepted"
                      : "remote.unconfirmed",
                  )
                }`,
              })),
            ]}
          />
        </div>
      )}
      {!requestId && (
        <form
          className="remote-command-form"
          onSubmit={(event) => {
            event.preventDefault();
            void submit();
          }}
        >
          <label htmlFor="remote-script">{t("remote.script")}</label>
          <textarea
            id="remote-script"
            required
            rows={5}
            maxLength={1_048_576}
            spellCheck={false}
            placeholder={device.os === "windows" ? "Get-Date" : "uname -a"}
            value={script}
            disabled={busy}
            onChange={(event) => setScript(event.target.value)}
          />
          <details className="remote-options">
            <summary>{t("remote.options")}</summary>
            <label htmlFor="remote-shell">{t("remote.shell")}</label>
            <Select
              id="remote-shell"
              value={shell}
              disabled={busy}
              onChange={setShell}
              options={(device.os === "windows"
                ? ["powershell", "pwsh", "cmd"]
                : ["sh", "bash", "zsh"]
              ).map((name) => ({ value: name, label: name }))}
            />
            <label htmlFor="remote-cwd">{t("remote.cwd")}</label>
            <input
              id="remote-cwd"
              type="text"
              value={cwd}
              disabled={busy}
              placeholder={t("remote.defaultCwd")}
              onChange={(event) => setCwd(event.target.value)}
            />
            <label htmlFor="remote-timeout">{t("remote.timeout")}</label>
            <input
              id="remote-timeout"
              type="number"
              min={1}
              max={86400}
              required
              value={timeout}
              disabled={busy}
              onChange={(event) =>
                setTimeoutSeconds(Number(event.target.value))
              }
            />
          </details>
          <div className="hero-actions">
            <button
              className="primary"
              type="submit"
              disabled={busy || !canRun}
            >
              <Icon name="play" />
              {t("remote.run")}
            </button>
          </div>
        </form>
      )}
      {requestId && (
        <div className="remote-result">
          <div className="remote-job-state" role="status">
            {job ? (
              <>
                {job.job_id} · {jobState(job)[0]}
              </>
            ) : (
              t(busy ? "remote.submitting" : "remote.unconfirmed")
            )}
            {job?.result?.exit_code != null && (
              <span>
                {t("remote.exitCode", { code: job.result.exit_code })}
              </span>
            )}
          </div>
          {job && (
            <pre
              className="remote-output"
              ref={outputLog}
              onScroll={(event) => {
                const log = event.currentTarget;
                followOutput.current =
                  log.scrollHeight - log.clientHeight - log.scrollTop < 24;
              }}
              aria-label={t("remote.output")}
              data-revision={outputRevision}
            >
              {outputBuffer.current.chunks.map((chunk) => (
                <span key={chunk.id} className={chunk.stream}>
                  {chunk.text}
                </span>
              ))}
            </pre>
          )}
          {outputBuffer.current.truncated && (
            <p className="field-help">{t("remote.truncated")}</p>
          )}
          {job?.output_loss_reason && (
            <p className="field-help">
              {t("remote.incomplete", { reason: job.output_loss_reason })}
            </p>
          )}
          {job?.error_message && (
            <p className="field-help">{job.error_message}</p>
          )}
          {!job && !busy && (
            <p className="field-help">{t("remote.recoverHint")}</p>
          )}
          <div className="hero-actions">
            <button
              disabled={busy || reading}
              onClick={() => void selectRequest(requestId)}
            >
              {t("remote.query")}
            </button>
            {job && isRunning(job) && (
              <button disabled={busy} onClick={() => void cancel()}>
                {t("remote.cancel")}
              </button>
            )}
            <button disabled={busy || reading} onClick={newCommand}>
              {t("remote.new")}
            </button>
          </div>
          <p className="field-help">{t("remote.backgroundHint")}</p>
        </div>
      )}
    </dialog>
  );
}
