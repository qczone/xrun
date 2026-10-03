import type { Job, Status } from "./api";

export const osName = (os: string | null | undefined) =>
  ({ macos: "macOS", windows: "Windows", linux: "Linux" })[os || ""] ||
  os ||
  "未知系统";
export const recordTime = (time: number) =>
  new Date(time).toLocaleString("zh-CN", { hour12: false });
export const isRunning = (job: Job) =>
  job.state === "starting" || job.state === "running";

export function duration(ms: number | null) {
  if (ms === null) return "—";
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)} 秒`;
  return `${Math.floor(ms / 60000)} 分 ${Math.floor((ms % 60000) / 1000)} 秒`;
}

export function serviceLabel(status: Status | null): [string, string] {
  if (!status) return ["检查状态", "正在读取本机状态…"];
  const { local, service } = status;
  if (!local.joined)
    return ["尚未加入", "创建或加入网络，让你的设备互相连接。"];
  if (service.approval_required)
    return ["等待授权", "需要允许 xrun 在后台运行。"];
  if (!local.daemon_running)
    return ["已停止", "后台服务已停止，其他设备暂时无法访问本机。"];
  if (local.remote_access_paused)
    return ["访问已暂停", "远程访问已暂停；已受理的后台任务继续运行。"];
  if (local.daemon_connected === true)
    return ["已连接", "后台服务运行中，已连接到中转。"];
  if (local.daemon_connected === null)
    return ["运行中", "旧版后台服务正在运行，更新后可查看实时连接状态。"];
  return ["连接中", "正在尝试连接中转，网络恢复后会自动重连。"];
}

export function taskState(job: Job): [string, string] {
  if (job.state === "exited")
    return job.exit_code === 0 && !job.signal
      ? ["执行成功", "success"]
      : ["异常退出", "failed"];
  const states = {
    starting: ["正在启动", "active"],
    running: ["运行中", "active"],
    failed: ["执行失败", "failed"],
    canceled: ["已取消", "neutral"],
    timed_out: ["已超时", "failed"],
    lost: ["结果丢失", "failed"],
  } satisfies Record<string, [string, string]>;
  return states[job.state];
}

export function commandText(job: Job) {
  return [job.program || "脚本", ...job.args]
    .map((arg) => (/[\s"'\\]/.test(arg) ? JSON.stringify(arg) : arg))
    .join(" ");
}

export function fileSize(size: number | null) {
  if (size === null) return "";
  if (size < 1024) return ` · ${size} B`;
  if (size < 1024 * 1024) return ` · ${(size / 1024).toFixed(1)} KiB`;
  return ` · ${(size / (1024 * 1024)).toFixed(1)} MiB`;
}
