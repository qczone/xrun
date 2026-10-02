"use strict";
const invoke = window.__TAURI__.core.invoke;
const $ = (id) => document.getElementById(id);
const labels = { overview: "本机状态", devices: "设备", history: "任务与日志", settings: "设置" };
let current, settingsData;
let busy = false, dirty = false, loadedDevices = false;
let deviceRows = [], renderedDevices = "";
let toastTimer;

function icon(name) {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "icon");
  const use = document.createElementNS(svg.namespaceURI, "use");
  use.setAttribute("href", "#i-" + name);
  svg.append(use);
  return svg;
}
function error(value) {
  $("error").textContent = value ? String(value) : "";
  $("error").hidden = !value;
}
function toast(message) {
  clearTimeout(toastTimer);
  $("toast").textContent = message;
  $("toast").hidden = false;
  toastTimer = setTimeout(() => { $("toast").hidden = true; }, 2600);
}
function page(name) {
  document.querySelectorAll(".page").forEach((node) => { node.hidden = node.id !== "page-" + name; });
  document.querySelectorAll("[data-page]").forEach((node) => {
    const selected = node.dataset.page === name;
    node.classList.toggle("selected", selected);
    if (selected) node.setAttribute("aria-current", "page"); else node.removeAttribute("aria-current");
  });
  $("breadcrumb").textContent = labels[name];
  document.querySelector("main").scrollTop = 0;
  if (name === "devices" && !loadedDevices && current?.local.joined) refreshDevices();
  if (name === "history") refreshHistory();
}
document.querySelectorAll("[data-page],[data-go]").forEach((node) => node.addEventListener("click", () => page(node.dataset.page || node.dataset.go)));

function confirmed(title, message) {
  $("confirm-title").textContent = title;
  $("confirm-message").textContent = message;
  const dialog = $("confirm");
  return new Promise((resolve) => {
    dialog.addEventListener("close", () => resolve(dialog.returnValue === "ok"), { once: true });
    dialog.showModal();
  });
}
function controls() {
  document.querySelectorAll("button,input,textarea").forEach((node) => {
    if (!node.closest("dialog") && !node.closest("#page-history") && !node.hasAttribute("data-page") && !node.hasAttribute("data-go")) node.disabled = busy || node.hasAttribute("data-fixed-disabled");
  });
  if (!current) return;
  const joined = current.local.joined;
  $("start").disabled = busy || current.local.daemon_running;
  $("stop").disabled = busy || !current.local.daemon_running;
  $("service-toggle").disabled = busy || !joined;
  for (const id of ["cwd", "concurrency", "path", "browse"]) $(id).disabled = busy || !joined;
  $("save").disabled = busy || !joined || !dirty;
  $("execution-unavailable").hidden = joined;
}
async function action(command, args = {}) {
  if (busy) return false;
  busy = true;
  controls();
  error(null);
  let success = false;
  try { await invoke(command, args); success = true; }
  catch (e) { error(e); }
  finally { busy = false; await refreshStatus(); controls(); }
  return success;
}
function stateLabel(local, service) {
  if (!local.joined) return ["尚未加入", "加入部署后，即可通过 xrun 访问已配对的设备。"];
  if (service.approval_required) return ["等待授权", "需要允许 xrun 在后台运行。"];
  if (!local.daemon_running) return ["已停止", "后台服务已停止，其他设备暂时无法访问本机。"];
  if (local.daemon_connected === true) return ["已连接", "后台服务运行中，已连接到 Server。"];
  if (local.daemon_connected === null) return ["运行中", "旧版后台服务正在运行，更新后可查看实时连接状态。"];
  return ["连接中", "正在尝试连接 Server，网络恢复后会自动重连。"];
}
async function refreshStatus() {
  try {
    current = await invoke("status");
    const { local, service } = current;
    const [badge, summary] = stateLabel(local, service);
    $("version").textContent = "v" + local.version;
    $("about-version").textContent = local.version;
    $("sidebar-name").textContent = local.name || "本机设备";
    $("sidebar-state").textContent = badge;
    $("sidebar-dot").classList.toggle("online", local.daemon_connected === true);
    $("device-name").textContent = local.name || "连接你的设备";
    $("identity-name").textContent = local.name || "";
    $("device-id").textContent = local.device_id || "";
    $("identity").hidden = !local.joined;
    $("join-section").hidden = local.joined;
    $("service-actions").hidden = !local.joined;
    $("approval").hidden = !service.approval_required;
    $("legacy").hidden = !service.legacy_installed;
    $("remove-row").hidden = !local.daemon_installed;
    $("autostart").checked = service.app_at_login;
    $("install-type").textContent = service.legacy_installed ? "CLI 安装的服务" : service.installed ? "App 后台服务" : "尚未安装服务";
    $("badge").textContent = badge;
    $("badge").classList.toggle("online", local.daemon_connected === true);
    $("settings-service-badge").textContent = badge;
    $("settings-service-badge").classList.toggle("online", local.daemon_connected === true);
    $("service-toggle").textContent = local.daemon_running ? "停止服务" : "启动服务";
    $("summary").textContent = summary;
    if (current.error) error(current.error);
    if (!local.joined) $("devices-message").textContent = "先加入部署，就能在这里查看其他设备。";
    controls();
    renderDevices();
  } catch (e) { error(e); }
}
function executionValue() {
  return {
    default_cwd: $("cwd").value.trim() ? $("cwd").value : null,
    max_concurrent_jobs: Number($("concurrency").value),
    path: $("path").value.trim() ? $("path").value : null
  };
}
function settingsChanged() {
  dirty = settingsData && JSON.stringify(executionValue()) !== JSON.stringify(settingsData.execution);
  $("save-status").textContent = dirty ? "有未保存的更改" : "保存后对新任务生效，无需重启服务。";
  $("save-status").classList.toggle("dirty", Boolean(dirty));
  controls();
}
async function loadSettings() {
  try {
    settingsData = await invoke("settings");
    $("cwd").value = settingsData.execution.default_cwd || "";
    $("cwd").placeholder = settingsData.home_dir;
    $("concurrency").value = settingsData.execution.max_concurrent_jobs;
    $("path").value = settingsData.execution.path || "";
    $("data-dir").value = settingsData.data_dir;
    const windows = settingsData.os === "windows";
    $("system-name").textContent = { macos: "macOS", windows: "Windows", linux: "Linux" }[settingsData.os] || settingsData.os;
    $("path-help").textContent = windows ? "多个目录用分号（;）分隔。留空继承后台服务的 PATH。" : "多个目录用冒号（:）分隔。留空继承后台服务的 PATH。";
    dirty = false; settingsChanged();
  } catch (e) { error(e); }
}

function renderDevices() {
  if (!current) return;
  const signature = JSON.stringify([deviceRows, current.local.device_id, current.allow_from, busy]);
  if (signature === renderedDevices) return;
  renderedDevices = signature;
  const list = $("devices");
  list.replaceChildren();
  const values = deviceRows.filter((d) => d.device_id !== current.local.device_id);
  $("device-count").textContent = values.length;
  $("device-count").hidden = !values.length;
  for (const device of values) {
    const row = document.createElement("div"); row.className = "device";
    const avatar = document.createElement("div"); avatar.className = "device-icon"; avatar.append(icon("monitor"));
    const details = document.createElement("div"); details.className = "device-details";
    const name = document.createElement("div"); name.className = "device-name"; name.textContent = device.name;
    const info = document.createElement("div"); info.className = "device-info";
    const dot = document.createElement("span"); dot.className = "dot" + (device.online && !device.revoked ? " online" : "");
    const osNames = { macos: "macOS", windows: "Windows", linux: "Linux" };
    const infoText = document.createElement("span"); infoText.textContent = (device.revoked ? "已撤销" : device.online ? "在线" : "离线") + " · " + (osNames[device.os] || device.os || "未知系统");
    info.append(dot, infoText);
    const id = document.createElement("div"); id.className = "device-id mono"; id.textContent = device.device_id;
    details.append(name, info, id);
    const label = document.createElement("label"); label.className = "switch";
    const input = document.createElement("input"); input.type = "checkbox"; input.disabled = busy || device.revoked;
    if (device.revoked) input.setAttribute("data-fixed-disabled", "");
    input.setAttribute("aria-label", "允许 " + device.name + " 访问本机");
    input.checked = current.allow_from.includes(device.device_id) && !device.revoked;
    const track = document.createElement("span"); track.className = "switch-track";
    input.addEventListener("change", async () => {
      const allow = input.checked;
      if (allow && !await confirmed("允许 " + device.name + " 访问本机？", "该设备将能以你的用户权限执行命令、传输文件和截图。请只授权你信任的设备。")) { input.checked = false; return; }
      if (await action("permission", { device: device.device_id, allow })) toast(allow ? "已允许访问本机" : "已取消访问权限");
    });
    label.append(input, track); row.append(avatar, details, label); list.append(row);
  }
}
async function refreshDevices() {
  if (!current?.local.joined) return;
  $("devices-message").hidden = false; $("devices-message").textContent = "正在查询设备…";
  $("refresh").disabled = true;
  try {
    const result = await invoke("devices");
    if (result.server_error) throw result.server_error.code + ": " + result.server_error.message;
    deviceRows = result.devices || [];
    deviceRows.sort((a, b) => Number(b.online) - Number(a.online) || a.name.localeCompare(b.name));
    loadedDevices = true;
    const others = deviceRows.filter((d) => d.device_id !== current.local.device_id);
    $("devices-message").textContent = "暂时没有其他设备。可以用 xrun invite 邀请新设备加入。";
    $("devices-message").hidden = others.length > 0;
    renderDevices();
  } catch (e) { $("devices-message").textContent = "无法查询设备：" + e; }
  finally { $("refresh").disabled = busy; }
}

$("join-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  await action("join", { link: $("link").value, name: $("name").value });
  if (current?.local.joined) { $("link").value = ""; await loadSettings(); await refreshDevices(); }
});
async function stopService() {
  if (await confirmed("停止后台服务？", "这会终止本机上由 xrun 运行的任务。其他设备将无法访问本机，直到你再次启动服务。")) await action("stop");
}
$("start").addEventListener("click", () => action("start"));
$("stop").addEventListener("click", stopService);
$("service-toggle").addEventListener("click", () => current?.local.daemon_running ? stopService() : action("start"));
$("remove").addEventListener("click", async () => {
  if (await confirmed("移除后台服务？", "这会停止服务和运行中的任务，并取消服务的登录启动。设备身份和数据会保留。")) await action("remove_service");
});
$("autostart").addEventListener("change", () => action("autostart", { enabled: $("autostart").checked }));
$("hide").addEventListener("click", () => action("hide_icon"));
$("refresh").addEventListener("click", refreshDevices);
for (const id of ["cwd", "concurrency", "path"]) $(id).addEventListener("input", settingsChanged);
$("browse").addEventListener("click", async () => {
  try { const path = await invoke("choose_directory"); if (path) { $("cwd").value = path; settingsChanged(); } }
  catch (e) { error(e); }
});
$("data-dir").addEventListener("focus", (event) => event.target.select());
$("execution-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  if (await action("save_settings", { execution: executionValue() })) { await loadSettings(); toast("执行环境已保存"); }
});
Promise.all([refreshStatus(), loadSettings()]).then(() => { if (current?.local.joined) refreshDevices(); });
setInterval(() => { if (!busy && !$("confirm").open) { refreshStatus(); pollHistory(); } }, 3000);
