# xrun

xrun 让开发者和 AI 在已授权的设备上执行程序、管理后台任务、传输单个文件和截图。设备主动连接自建的 Linux Server；CLI、Server 和 daemon 使用同一个 Rust 二进制。

Server 只负责身份与会话转发；任务、日志和去重记录保存在目标 daemon。代码同步使用 Git，文件在本地用自己的编辑工具修改。

## 构建

```bash
cargo build --locked --release
```

将 `target/release/xrun`（Windows 为 `xrun.exe`）放到固定目录并加入 PATH，再安装服务。CLI、Server 和 daemon 的完整发布版本必须一致。

GitHub Actions 的 `Package` 工作流可手动构建 Linux x86_64、Windows x86_64 和 macOS Apple Silicon 压缩包；macOS 签名、公证需要配置已有工作流列出的凭据。

## 部署和加入

在 Linux Server 上执行：

```bash
xrun up --addr 192.168.1.10:9528
```

替换为其他设备可访问的 IPv4 地址或有 A 记录的域名，并放行 TCP 9528。省略 `--addr` 时自动探测地址；手动地址持久保留，重复 `up` 不会覆盖。Server 主机注册为管理设备 `admin`，默认同时启动本机 daemon。

`up` 默认安装 systemd 用户服务并启用 linger。没有 systemd 时使用前台模式：

```bash
xrun up --addr 192.168.1.10:9528 --no-service --no-daemon
# 另一个终端运行本机 daemon（如果需要接受远端调用）
xrun daemon
```

在另一台设备使用 `up` 输出的完整邀请链接，例如 Windows：

```bash
xrun join 'xrun://192.168.1.10:9528/<CA指纹>#<Token>' --name win1
xrun status
```

邀请有效期 10 分钟，首次加入默认与邀请方相互授权。其他设备间需要分别授权；邀请方 daemon 离线时，在邀请方执行 `xrun allow-from win1`。生成新邀请用 `xrun invite`；只注册、不相互授权用 `xrun invite --no-allow`。

`join` 安装当前用户的 daemon：Linux 使用 systemd，macOS 使用 LaunchAgent，Windows 使用登录计划任务。只安装 CLI 身份可加 `--no-daemon`，之后用 `xrun daemon install` 安装服务，或 `xrun daemon` 前台运行。

## 日常命令

在已授权的调用设备上运行：

```bash
# 构建、测试：直接传程序和参数
xrun win1 -C 'D:\demo' -- cargo build
xrun win1 -C 'D:\demo' -- cargo test

# 启动应用，受理后输出实际任务引用，例如 win1/ABC123
xrun win1 start -C 'D:\demo' -- 'target\debug\demo.exe'
xrun win1 jobs --running
xrun win1 logs ABC123 --follow
xrun win1 screenshot ./screen.png
xrun win1 kill ABC123
xrun win1 wait ABC123

# 文件路径按源、目标排列
xrun win1 pull 'D:\demo\src\main.rs' ./main.rs --json
# 使用本地编辑工具修改 main.rs，再上传
xrun win1 push ./main.rs 'D:\demo\src\main.rs'

# 目录查询和无头浏览器截图由远端现有工具完成
xrun win1 -- cmd /c dir 'D:\demo'
xrun win1 -C 'D:\demo' -- node scripts/screenshot.js
xrun win1 pull 'D:\demo\artifacts\page.png' ./page.png
```

后续命令中的 `ABC123` 替换为实际返回的任务 ID。无需先生成 UUID 或用 Shell 临时变量保存提交。

执行默认超时 30 分钟，`start` 默认不限时；用 `--timeout 秒` 覆盖，0 表示不限时。`wait --timeout 秒` 只限制等待，不取消任务。`--env KEY=VALUE` 可重复指定；`--stdin` 透传输入，最多 1 MiB。

执行不会隐式启动 Shell。脚本从 stdin 读取，Shell 名称必填，支持 sh、bash、zsh、powershell、pwsh 和 cmd。PowerShell 脚本使用 UTF-8，cmd 脚本仅接受 ASCII：

```bash
xrun linux1 -C /repo --script bash < ./build.sh
xrun win1 -C 'D:\demo' --script powershell < ./build.ps1
```

push、pull 只传单个文件，最多 64 MiB，不转换 BOM 或换行。push 两条路径必填；pull 省略本地目标时保存到唯一临时文件。`-` 表示本地 stdin/stdout：

```bash
xrun linux1 pull /repo/config.json -
printf '%s\n' 'hello' | xrun linux1 push - /repo/note.txt
xrun linux1 push ./config.json /repo/config.json --expect '<pull返回的sha256>'
```

`--expect` 可防止覆盖修改后的目标；`--no-overwrite` 要求目标不存在，两者互斥。截图保存 PNG，省略目标路径时使用唯一临时文件。Linux 截图仅支持 X11；macOS 需要屏幕录制权限，Windows 需要可访问的交互桌面。网页截图可以在无桌面的设备上通过项目的无头浏览器脚本生成，再用 pull 取回。

## 授权、查询和停止

```bash
xrun status
xrun win1 info
xrun win1 jobs
xrun win1 jobs ABC123 --json
xrun recent --json
xrun guide

# 在被控设备上执行，指定允许控制本机的来源
xrun allow-from mac1
xrun deny-from mac1

# 仅管理设备可以撤销身份
xrun revoke win1

xrun daemon uninstall
xrun down
```

设备名是别名，认证、白名单和去重使用不可变设备 ID；命令也接受 `dev_…`。deny 不取消已有任务；revoke 切断该身份的连接，也不撤销已经发生的副作用。daemon 以当前用户权限运行。

配置、身份和数据库在 `~/.xrun`（Windows 为 `%USERPROFILE%\.xrun`）。工具链 PATH、默认目录和来源白名单写入 `daemon.toml`：

```toml
allow_from = ["dev_来源设备ID"]
default_cwd = "/path/to/repo"
max_concurrent_jobs = 4

[env]
PATH = "/usr/local/bin:/usr/bin:/bin"
```

`xrun guide` 直接输出本 README 的使用说明。

`down` 移除本机服务、保留数据；`down --purge` 经终端确认后删除本机数据。`daemon reset` 要求 daemon 已停止，明确重建任务数据库，保留设备身份。

## 断线与结果确认

前台执行原样输出远端 stdout/stderr，并保留远端退出码。超时为 124，本地参数错误为 2，连接、身份或执行层错误为 125。日志不完整会注明原因。

返回 **75 表示结果未确认**。已知任务 ID 就查询 jobs 或 wait；只有请求 ID 时：

```bash
xrun recent
xrun win1 jobs --request-id '<request_id>'
```

恢复同一次提交时提供原参数、原输入和原 `--request-id`。daemon 按来源设备与请求 ID 去重，不重新运行已受理任务；数据库重建后返回 `DB_RESET`。普通调用会生成新请求 ID。

CLI 或 Server 断开时任务继续运行；daemon 重启后未完成任务标记为 lost，不重跑。push 响应丢失时不自动重发，先 pull 确认目标内容。

## 验证

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
# Linux X11 截图测试（需安装 Xvfb、libX11、libXrandr）
xvfb-run -a -s '-screen 0 1024x768x24' cargo test --locked --test screenshot -- --ignored
```

已在 macOS 和 Linux 容器运行 TLS 配对、执行、后台任务、去重、并发、文件传输、撤销与故障恢复测试，并在 Linux Xvfb 中验证 PNG 截图。Windows 已通过交叉类型检查；[Test Linux](.github/workflows/test-linux.yml)、[Test macOS](.github/workflows/test-macos.yml) 和 [Test Windows](.github/workflows/test-windows.yml) 分别在对应平台原生运行测试。测试直接调用 Server 库以覆盖转发，公开的 Server CLI 仍仅支持 Linux。

系统服务安装、Windows 原生进程行为、真实桌面截图权限/锁屏、设备睡眠和磁盘故障仍需对应环境验收。详细设计和验收清单见 [design.md](design.md)。
