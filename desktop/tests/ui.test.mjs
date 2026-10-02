import { readFileSync } from "node:fs";
import { Script } from "node:vm";
import { setImmediate } from "node:timers/promises";
import assert from "node:assert/strict";
import test from "node:test";
import { JSDOM } from "jsdom";

const html = readFileSync(new URL("../ui/index.html", import.meta.url), "utf8");
const source = readFileSync(new URL("../ui/app.js", import.meta.url), "utf8");
const historySource = readFileSync(new URL("../ui/history.js", import.meta.url), "utf8");
async function fixture(handlers = {}) {
  const dom = new JSDOM(html, { runScripts: "outside-only", url: "https://xrun.test/" });
  const calls = [];
  const status = {
    local: { joined: true, device_id: "self", name: "mac1", version: "0.0.1-beta.1", daemon_running: true, daemon_connected: true, daemon_installed: true },
    service: { app_at_login: false, installed: true, legacy_installed: false, approval_required: false },
    allow_from: [], error: null
  };
  const settings = {
    execution: { default_cwd: null, max_concurrent_jobs: 4, path: null },
    home_dir: "/Users/test", data_dir: "/Users/test/.xrun", os: "macos"
  };
  dom.window.setInterval = () => 0;
  dom.window.TextDecoder = TextDecoder;
  dom.window.__TAURI__ = { core: { invoke: async (name, args) => {
    calls.push({ name, args });
    if (handlers[name]) return handlers[name](args);
    if (name === "status") return structuredClone(status);
    if (name === "settings") return structuredClone(settings);
    if (name === "devices") return { devices: [
      { device_id: "unsafe-label", name: '<img id="injected" src=x onerror=alert(1)>', online: true, revoked: false, os: "windows" },
      { device_id: "revoked", name: "old-device", online: false, revoked: true, os: "linux" }
    ] };
    if (name === "save_settings") settings.execution = args.execution;
    return null;
  } } };
  new Script(source).runInContext(dom.getInternalVMContext());
  new Script(historySource).runInContext(dom.getInternalVMContext());
  await setImmediate(); await setImmediate();
  return { dom, calls, status, $: (id) => dom.window.document.getElementById(id) };
}
test("settings save only execution fields and keep unsaved input while polling", async () => {
  const { dom, calls, $ } = await fixture();
  try {
    dom.window.document.querySelector('[data-page="settings"]').click();
    const input = $("concurrency");
    input.value = "8";
    input.dispatchEvent(new dom.window.Event("input", { bubbles: true }));
    await dom.window.refreshStatus();
    assert.equal(input.value, "8");
    assert.equal($("save").disabled, false);
    assert.equal($("page-settings").hidden, false);
    $("execution-form").dispatchEvent(new dom.window.Event("submit", { bubbles: true, cancelable: true }));
    await setImmediate(); await setImmediate();
    assert.deepEqual(JSON.parse(JSON.stringify(calls.find((c) => c.name === "save_settings").args)), {
      execution: { default_cwd: null, max_concurrent_jobs: 8, path: null }
    });
    assert.equal($("save").disabled, true);
    assert.equal($("toast").textContent, "执行环境已保存");
  } finally { dom.window.close(); }
});

function task(id = "ABC123", changes = {}) {
  return {
    job_id: id, db_id: "db-original", source_device_id: "unsafe-label", target_device_id: "self",
    program: "echo", args: ['<img id="command-injected" src=x>'], cwd: "/repo", state: "running",
    exit_code: null, signal: null, duration_ms: null, last_seq: 3, output_complete: true,
    incomplete_reason: null, error: null, created_at_ms: Date.now() - 1000, updated_at_ms: Date.now(),
    leftover_possible: false, ...changes
  };
}
const event = (seq, stream, bytes) => ({ seq, stream, data_base64: Buffer.from(bytes).toString("base64") });
test("task output follows sequences, decodes split UTF-8 and displays failures as plain text", async () => {
  let reads = 0;
  const { dom, calls, $ } = await fixture({
    task_history: () => ({ db_id: "db-original", jobs: [task()], next_cursor: null }),
    task_output: () => ++reads === 1 ? {
      job: task(), has_more: false, events: [event(1, "stdout", [0xe4, 0xb8]), event(2, "stderr", "<img id=log-injected>\n"), event(3, "stdout", [0xad])]
    } : {
      job: task("ABC123", { state: "exited", exit_code: 7, duration_ms: 1234, last_seq: 4, output_complete: false, incomplete_reason: "TRUNCATED" }),
      has_more: false, events: [event(3, "stdout", [0xad]), event(4, "stderr", "\u001b[31mfailed\u001b[0m\n")]
    }
  });
  try {
    dom.window.document.querySelector('[data-page="history"]').click();
    await setImmediate();
    $("history-records").querySelector("button").click();
    await setImmediate();
    assert.equal($("task-detail").hidden, false);
    assert.match($("task-log").textContent, /中/);
    assert.doesNotMatch($("task-log").textContent, /�/);
    assert.equal(dom.window.document.getElementById("command-injected"), null);
    assert.equal(dom.window.document.getElementById("log-injected"), null);
    await dom.window.loadTaskOutput();
    assert.equal($("task-log").textContent.match(/中/g).length, 1);
    assert.equal($("task-state").textContent, "异常退出");
    assert.equal($("task-exit").textContent, "退出码 7");
    assert.match($("task-warning").textContent, /输出已被截断/);
    assert.doesNotMatch($("task-log").textContent, /\u001b/);
    const requests = calls.filter((call) => call.name === "task_output");
    assert.equal(requests[1].args.after, 3);
    assert.equal(requests[1].args.dbId, "db-original");
    $("output-stream").value = "stderr";
    $("output-stream").dispatchEvent(new dom.window.Event("change"));
    assert.equal($("task-log").dataset.filter, "stderr");
    await dom.window.pollHistory();
    assert.equal(reads, 2);
  } finally { dom.window.close(); }
});

test("file records show interrupted transfers safely without fetching remote devices", async () => {
  const { dom, calls, $ } = await fixture({
    task_history: () => ({ db_id: null, jobs: [], next_cursor: null }),
    file_history: () => ({ entries: [{ time_ms: Date.now(), source_device_id: "remote", op: "push", path: '<img id="file-injected">', size: 2048, result: "failed_or_disconnected" }], next_cursor: null })
  });
  try {
    dom.window.document.querySelector('[data-page="history"]').click();
    await setImmediate();
    dom.window.document.querySelector('[data-history-tab="files"]').click();
    await setImmediate();
    assert.match($("history-records").textContent, /接收文件.*失败或中断/);
    assert.match($("history-records").textContent, /2.0 KiB/);
    assert.equal(dom.window.document.getElementById("file-injected"), null);
    assert.equal($("task-filter-label").hidden, true);
    assert.equal(calls.find((call) => call.name === "file_history").args.before, null);
  } finally { dom.window.close(); }
});

test("late output cannot replace another task and database reset stops automatic reads", async () => {
  let completeOld, reads = 0;
  const pending = new Promise((resolve) => { completeOld = resolve; });
  const { dom, $ } = await fixture({
    task_output: ({ job }) => {
      reads++;
      if (job === "OLD") return pending;
      throw "DB_RESET: database replaced";
    }
  });
  try {
    dom.window.selectTask(task("OLD"), "db-original");
    dom.window.selectTask(task("NEW"), "db-original");
    await setImmediate();
    completeOld({ job: task("OLD"), events: [event(1, "stdout", "old output")], has_more: false });
    await setImmediate();
    assert.equal($("task-title").textContent, "任务 NEW");
    assert.doesNotMatch($("task-log").textContent, /old output/);
    assert.match($("history-error").textContent, /DB_RESET/);
    dom.window.document.querySelector('[data-page="history"]').click();
    await setImmediate();
    const previous = reads;
    await dom.window.pollHistory();
    assert.equal(reads, previous);
  } finally { dom.window.close(); }
});
test("remote names remain text and revoked devices stay disabled during settings edits", async () => {
  const { dom, $ } = await fixture();
  try {
    assert.equal(dom.window.document.getElementById("injected"), null);
    assert.match($("devices").textContent, /<img id="injected"/);
    const revoked = dom.window.document.querySelector('[aria-label="允许 old-device 访问本机"]');
    assert.equal(revoked.disabled, true);
    $("cwd").value = "/repo";
    $("cwd").dispatchEvent(new dom.window.Event("input", { bubbles: true }));
    assert.equal(revoked.disabled, true);
  } finally { dom.window.close(); }
});
