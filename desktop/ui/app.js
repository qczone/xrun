"use strict";
const invoke = window.__TAURI__.core.invoke;
const $ = (id) => document.getElementById(id);
let current;
let busy = false;
let deviceRows = [];
let renderedDevices = "";

function error(value) {
  $("error").textContent = value ? String(value) : "";
  $("error").hidden = !value;
}
function confirmed(title, message) {
  $("confirm-title").textContent = title;
  $("confirm-message").textContent = message;
  const dialog = $("confirm");
  return new Promise((resolve) => {
    dialog.addEventListener("close", () => resolve(dialog.returnValue === "ok"), { once: true });
    dialog.showModal();
  });
}
async function action(command, args = {}) {
  if (busy) return;
  busy = true;
  document.querySelectorAll("button,input").forEach((node) => { if (!node.closest("dialog")) node.disabled = true; });
  error(null);
  try { await invoke(command, args); }
  catch (e) { error(e); }
  finally {
    busy = false;
    document.querySelectorAll("button,input").forEach((node) => node.disabled = false);
    await refreshStatus();
  }
}
async function refreshStatus() {
  try {
    current = await invoke("status");
    const { local, service } = current;
    $("version").textContent = local.version;
    $("device-name").textContent = local.name || "连接你的设备";
    $("device-id").textContent = local.device_id || "";
    $("identity").hidden = !local.joined;
    $("join-section").hidden = local.joined;
    $("devices-section").hidden = !local.joined;
    $("service-actions").hidden = !local.joined;
    $("approval").hidden = !service.approval_required;
    $("legacy").hidden = !service.legacy_installed;
    $("remove-row").hidden = !local.daemon_installed;
    $("autostart").checked = service.app_at_login;
    let badge = "尚未加入";
    let summary = "加入部署后，即可通过 xrun 访问已配对的设备。";
    if (local.joined) {
      if (service.approval_required) { badge = "等待授权"; summary = "需要允许 xrun 在后台运行。"; }
      else if (!local.daemon_running) { badge = "已停止"; summary = "后台服务已停止，其他设备暂时无法访问本机。"; }
      else if (local.daemon_connected === true) { badge = "已连接"; summary = "后台服务运行中，已连接到 Server。"; }
      else if (local.daemon_connected === null) { badge = "运行中"; summary = "旧版后台服务正在运行，更新后可查看实时连接状态。"; }
      else { badge = "连接中"; summary = "后台服务正在尝试连接 Server，网络恢复后会自动重连。"; }
    }
    $("badge").textContent = badge;
    $("badge").classList.toggle("online", local.daemon_connected === true);
    $("summary").textContent = summary;
    $("start").disabled = busy || local.daemon_running;
    $("stop").disabled = busy || !local.daemon_running;
    if (current.error) error(current.error);
    renderDevices();
  } catch (e) { error(e); }
}
function renderDevices() {
  const signature = JSON.stringify([deviceRows, current.local.device_id, current.allow_from, busy]);
  if (signature === renderedDevices) return;
  renderedDevices = signature;
  const list = $("devices");
  list.replaceChildren();
  for (const device of deviceRows) {
    if (device.device_id === current.local.device_id || device.revoked) continue;
    const row = document.createElement("div"); row.className = "device";
    const details = document.createElement("div");
    const name = document.createElement("div"); name.className = "device-name"; name.textContent = device.name;
    const info = document.createElement("div"); info.className = "device-info"; info.textContent = `${device.online ? "在线" : "离线"} · ${device.os || "未知系统"}`;
    details.append(name, info);
    const label = document.createElement("label"); label.className = "device-action";
    const text = document.createElement("span"); text.textContent = "允许访问本机";
    const input = document.createElement("input"); input.type = "checkbox"; input.disabled = busy;
    input.checked = current.allow_from.includes(device.device_id);
    input.addEventListener("change", async () => {
      const allow = input.checked;
      if (allow && !await confirmed(`允许 ${device.name} 访问本机？`, "该设备将能以你的用户权限执行命令、传输文件和截图。请只授权你信任的设备。")) { input.checked = false; return; }
      await action("permission", { device: device.device_id, allow });
    });
    label.append(text, input); row.append(details, label); list.append(row);
  }
}
async function refreshDevices() {
  $("devices-message").textContent = "正在查询…";
  $("refresh").disabled = true;
  try {
    const result = await invoke("devices");
    if (result.server_error) throw `${result.server_error.code}: ${result.server_error.message}`;
    deviceRows = result.devices || [];
    $("devices-message").textContent = deviceRows.filter((d) => d.device_id !== current.local.device_id && !d.revoked).length ? "" : "暂时没有其他设备。";
    renderDevices();
  } catch (e) { $("devices-message").textContent = `无法查询设备：${e}`; }
  finally { $("refresh").disabled = busy; }
}
$("join-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  await action("join", { link: $("link").value, name: $("name").value });
  if (current?.local.joined) { $("link").value = ""; await refreshDevices(); }
});
$("start").addEventListener("click", () => action("start"));
$("stop").addEventListener("click", async () => {
  if (await confirmed("停止后台服务？", "这会终止本机上由 xrun 运行的任务。其他设备将无法访问本机，直到你再次启动服务。")) await action("stop");
});
$("remove").addEventListener("click", async () => {
  if (await confirmed("移除后台服务？", "这会停止服务和运行中的任务，并取消服务的登录启动。设备身份和数据会保留。")) await action("remove_service");
});
$("autostart").addEventListener("change", () => action("autostart", { enabled: $("autostart").checked }));
$("hide").addEventListener("click", () => action("hide_icon"));
$("refresh").addEventListener("click", refreshDevices);
refreshStatus().then(() => { if (current?.local.joined) refreshDevices(); });
setInterval(() => { if (!busy && !$("confirm").open) refreshStatus(); }, 3000);
