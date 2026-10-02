"use strict";
let historyTab = "tasks", historyCursors = [null], historyIndex = 0, historyNext = null;
let historyLoading = false, historyRequest = 0, historySignature = "";
let selectedTask = null;
const OUTPUT_LIMIT = 1024 * 1024;

function historyError(value) {
  $("history-error").textContent = value ? String(value) : "";
  $("history-error").hidden = !value;
}
function historyControls() {
  $("history-refresh").disabled = historyLoading || Boolean(selectedTask?.loading);
  $("history-prev").disabled = historyLoading || historyIndex === 0;
  $("history-next").disabled = historyLoading || historyNext === null;
  $("history-page-number").textContent = "第 " + (historyIndex + 1) + " 页";
}
function deviceLabel(id) {
  if (id === current?.local.device_id) return current.local.name || id;
  return deviceRows.find((device) => device.device_id === id)?.name || id;
}
function recordTime(time) {
  return new Date(time).toLocaleString("zh-CN", { hour12: false });
}
function duration(ms) {
  if (ms === null || ms === undefined) return "—";
  if (ms < 1000) return ms + " ms";
  if (ms < 60000) return (ms / 1000).toFixed(1) + " 秒";
  return Math.floor(ms / 60000) + " 分 " + Math.floor(ms % 60000 / 1000) + " 秒";
}
function taskState(job) {
  const states = { starting: ["正在启动", "active"], running: ["运行中", "active"], failed: ["执行失败", "failed"], canceled: ["已取消", "neutral"], timed_out: ["已超时", "failed"], lost: ["结果丢失", "failed"] };
  if (job.state === "exited") return job.exit_code === 0 && !job.signal ? ["执行成功", "success"] : ["异常退出", "failed"];
  return states[job.state] || [job.state, "neutral"];
}
function commandText(job) {
  return [job.program || "脚本", ...job.args].map((arg) => /[\s"'\\]/.test(arg) ? JSON.stringify(arg) : arg).join(" ");
}
function textNode(tag, className, value) {
  const node = document.createElement(tag);
  node.className = className;
  node.textContent = value;
  return node;
}
function stateChip(label, kind) {
  return textNode("span", "task-state " + kind, label);
}

async function refreshHistory() {
  if (selectedTask) return loadTaskOutput();
  return loadHistoryPage();
}
async function pollHistory() {
  if ($("page-history").hidden || $("confirm").open) return;
  if (selectedTask) {
    if (!selectedTask.finished || selectedTask.more) await loadTaskOutput();
  } else if (!historyLoading) await loadHistoryPage();
}
async function loadHistoryPage() {
  const token = ++historyRequest;
  const tab = historyTab;
  historyLoading = true; historyControls(); historyError(null);
  try {
    const args = { before: historyCursors[historyIndex] };
    if (tab === "tasks") args.filter = $("task-filter").value;
    const result = await invoke(tab === "tasks" ? "task_history" : "file_history", args);
    if (token !== historyRequest || selectedTask) return;
    historyNext = result.next_cursor;
    const values = tab === "tasks" ? result.jobs : result.entries;
    const signature = JSON.stringify([tab, values, deviceRows.map((d) => [d.device_id, d.name]), result.db_id]);
    if (historySignature !== signature) {
      historySignature = signature;
      const list = $("history-records"); list.replaceChildren();
      for (const record of values) list.append(tab === "tasks" ? taskRow(record, result.db_id) : fileRow(record));
    }
    $("history-empty").hidden = values.length > 0;
    $("history-empty").textContent = tab === "files" ? "暂无文件操作或截图记录。" : $("task-filter").value === "all" ? "暂无任务。通过 xrun 在本机执行的任务会出现在这里。" : "暂无符合状态的任务。";
  } catch (e) { if (token === historyRequest) { historyError(e); $("history-empty").hidden = true; } }
  finally { if (token === historyRequest) { historyLoading = false; historyControls(); } }
}
function taskRow(job, dbId) {
  const row = document.createElement("button"); row.className = "task-record";
  const [label, kind] = taskState(job);
  const header = document.createElement("span"); header.className = "record-header";
  header.append(textNode("strong", "mono", job.job_id), stateChip(label, kind), textNode("span", "record-time", recordTime(job.created_at_ms)));
  const command = textNode("span", "record-command mono", commandText(job));
  const meta = textNode("span", "record-meta", "来源：" + deviceLabel(job.source_device_id) + " · " + (job.duration_ms === null ? "点击查看输出" : "耗时 " + duration(job.duration_ms)));
  row.append(header, command, meta);
  row.addEventListener("click", () => selectTask(job, dbId));
  return row;
}
function fileRow(record) {
  const row = document.createElement("article"); row.className = "file-record";
  const header = document.createElement("div"); header.className = "record-header";
  const labels = { push: "接收文件", pull: "发送文件", screenshot: "截图" };
  header.append(textNode("strong", "", labels[record.op] || record.op), stateChip(record.result === "ok" ? "已完成" : "失败或中断", record.result === "ok" ? "success" : "failed"), textNode("span", "record-time", recordTime(record.time_ms)));
  row.append(header);
  if (record.path) row.append(textNode("p", "record-command mono", record.path));
  const size = record.size === null ? "" : " · " + (record.size < 1024 ? record.size + " B" : record.size < 1024 * 1024 ? (record.size / 1024).toFixed(1) + " KiB" : (record.size / (1024 * 1024)).toFixed(1) + " MiB");
  row.append(textNode("p", "record-meta", "来源：" + deviceLabel(record.source_device_id) + size));
  return row;
}
function resetHistoryList() {
  historyCursors = [null]; historyIndex = 0; historyNext = null; historySignature = "";
  return loadHistoryPage();
}
function selectTask(job, dbId) {
  historyRequest++; historyLoading = false;
  selectedTask = { jobId: job.job_id, dbId, after: null, decoders: new Map(), nodes: [], length: 0, loading: false, more: false, finished: false, flushed: false };
  $("history-list-view").hidden = true; $("task-detail").hidden = false;
  $("task-log").replaceChildren($("log-empty")); $("log-empty").hidden = false;
  $("output-limit").hidden = true; $("output-status").textContent = "正在读取输出…";
  $("output-stream").value = "all"; $("task-log").dataset.filter = "all";
  historyError(null); renderTask(job); loadTaskOutput();
}
function renderTask(job) {
  const [label, kind] = taskState(job);
  $("task-title").textContent = "任务 " + job.job_id;
  $("task-state").textContent = label; $("task-state").className = "task-state " + kind;
  $("task-command").textContent = commandText(job);
  const source = deviceLabel(job.source_device_id);
  $("task-source").textContent = source === job.source_device_id ? source : source + " · " + job.source_device_id;
  $("task-cwd").textContent = job.cwd;
  $("task-created").textContent = recordTime(job.created_at_ms);
  const running = job.state === "starting" || job.state === "running";
  $("task-duration").textContent = job.duration_ms !== null ? duration(job.duration_ms) : running ? "约 " + duration(Math.max(0, Date.now() - job.created_at_ms)) : "—";
  $("task-exit").textContent = job.signal ? "信号 " + job.signal : job.exit_code !== null ? "退出码 " + job.exit_code : running ? "尚未结束" : "无退出码";
  const reasons = { TRUNCATED: "输出已被截断。", LOG_EXPIRED: "输出已按保留规则清理。", DETACHED_OUTPUT: "后台子进程的部分输出未能收集。" };
  const warnings = [];
  if (job.error) warnings.push(job.error);
  if (!job.output_complete) warnings.push(reasons[job.incomplete_reason] || "输出不完整：" + (job.incomplete_reason || "原因未知"));
  if (job.leftover_possible) warnings.push("可能仍有未清理的子进程。");
  if (running && current && !current.local.daemon_running) warnings.push("后台服务已停止，记录中的任务状态可能尚未更新。");
  $("task-warning").textContent = warnings.join(" "); $("task-warning").hidden = !warnings.length;
}
function appendOutput(target, stream, value) {
  // Output is plain text; neither HTML nor terminal control sequences are executed.
  const text = value.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, "").replace(/[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]/g, "");
  if (!text) return;
  const node = textNode("span", stream === "stderr" ? "output-stderr" : "", text);
  node.dataset.stream = stream;
  $("task-log").append(node); target.nodes.push(node); target.length += text.length;
  $("log-empty").hidden = true;
  while (target.length > OUTPUT_LIMIT || target.nodes.length > 2000) {
    const old = target.nodes.shift(); target.length -= old.textContent.length; old.remove();
    $("output-limit").hidden = false;
  }
}
async function loadTaskOutput() {
  const target = selectedTask;
  if (!target || target.loading) return;
  target.loading = true; historyControls();
  try {
    const result = await invoke("task_output", { dbId: target.dbId, job: target.jobId, after: target.after });
    if (selectedTask !== target) return;
    historyError(null); renderTask(result.job);
    const log = $("task-log");
    if (target.after === null && result.events[0]?.seq > 1) $("output-limit").hidden = false;
    for (const event of result.events) {
      if (target.after !== null && event.seq <= target.after) continue;
      const stream = event.stream === "stderr" ? "stderr" : "stdout";
      if (!target.decoders.has(stream)) target.decoders.set(stream, new TextDecoder());
      const bytes = Uint8Array.from(atob(event.data_base64), (c) => c.charCodeAt(0));
      appendOutput(target, stream, target.decoders.get(stream).decode(bytes, { stream: true }));
      target.after = event.seq;
    }
    if (target.after === null) target.after = result.job.last_seq;
    target.more = result.has_more;
    target.finished = !["starting", "running"].includes(result.job.state);
    if (target.finished && !target.more && !target.flushed) {
      for (const [stream, decoder] of target.decoders) appendOutput(target, stream, decoder.decode());
      target.flushed = true;
    }
    $("output-status").textContent = target.more ? "正在补读输出…" : target.finished ? "任务已结束" : "每 3 秒更新输出";
    if ($("output-follow").checked) log.scrollTop = log.scrollHeight;
  } catch (e) {
    if (selectedTask === target) {
      target.more = false;
      historyError(e); $("output-status").textContent = "输出读取失败";
      if (String(e).startsWith("DB_RESET") || String(e).startsWith("JOB_NOT_FOUND") || String(e).startsWith("DB_MISSING")) target.finished = true;
    }
  } finally {
    target.loading = false; historyControls();
    if (selectedTask === target && target.more && !$("page-history").hidden) queueMicrotask(loadTaskOutput);
  }
}

document.querySelectorAll("[data-history-tab]").forEach((button) => button.addEventListener("click", () => {
  historyTab = button.dataset.historyTab;
  document.querySelectorAll("[data-history-tab]").forEach((node) => {
    const selected = node === button; node.classList.toggle("selected", selected); node.setAttribute("aria-pressed", String(selected));
  });
  $("task-filter-label").hidden = historyTab !== "tasks";
  resetHistoryList();
}));
$("task-filter").addEventListener("change", resetHistoryList);
$("history-refresh").addEventListener("click", refreshHistory);
$("history-prev").addEventListener("click", () => { if (historyIndex > 0) { historyIndex--; loadHistoryPage(); } });
$("history-next").addEventListener("click", () => { if (historyNext !== null) { historyIndex++; historyCursors[historyIndex] = historyNext; loadHistoryPage(); } });
$("history-back").addEventListener("click", () => {
  selectedTask = null; $("task-detail").hidden = true; $("history-list-view").hidden = false;
  historyError(null); historySignature = ""; loadHistoryPage();
});
$("output-stream").addEventListener("change", () => { $("task-log").dataset.filter = $("output-stream").value; });
historyControls();
