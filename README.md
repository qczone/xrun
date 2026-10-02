# xrun

xrun 让开发者和 AI 在已授权的设备上执行程序、管理后台任务、传输单个文件和截图。设备主动连接自建的 Linux Server；CLI、Server 和 daemon 使用同一个 Rust 二进制。

Server 只负责身份与会话转发；任务、日志和去重记录保存在目标 daemon。代码同步使用 Git，文件在本地用自己的编辑工具修改。

## 构建

```bash
cargo build --locked --release
```

将 `target/release/xrun`（Windows 为 `xrun.exe`）放到固定目录并加入 PATH，再安装服务。CLI、Server 和 daemon 的完整发布版本必须一致。

GitHub Actions 的 `Package` 工作流可手动构建三个平台的 CLI 压缩包，以及 macOS Apple Silicon App 和 Windows x86_64 用户级安装包；macOS 签名、公证需要配置工作流列出的凭据。

## 桌面 App

macOS 13 及以上将 `xrun.app` 放入 `/Applications`，Windows 运行安装包。打开 App 后可以粘贴邀请链接加入部署、查看连接状态、控制后台服务，以及允许其他设备访问本机。已有 CLI 身份和配置会直接复用。

App 分为本机状态、设备、任务与日志、设置四个页面。「任务与日志」展示在本机执行的任务，可以按状态筛选，查看来源设备、命令、工作目录、耗时、退出结果和 stdout/stderr；运行中的输出每 3 秒刷新，界面显示最近的输出。文件与截图页展示 push、pull 和 screenshot 的已有操作记录。App 只读本机 daemon 数据库，服务停止或网络断开后仍可查询，不会查询其他设备上执行的任务。

设置中可以选择默认工作目录、修改任务并发上限和工具搜索路径 PATH；保存后对新任务生效，无需重启。其他环境变量和已有权限保留。

已加入时，启动 App 默认显示菜单栏或系统托盘图标；点击图标打开管理窗口。关闭窗口会留在托盘。“隐藏图标”同时隐藏窗口，重新打开 App 会恢复图标和窗口。App 只有一个实例，重新打开不会启动第二个 daemon。

“退出 App”只退出界面。后台服务独立运行，随用户登录启动；“停止服务”正常结束 daemon 和本机运行中的任务，保留任务结果，服务会在下次登录时再启动。“移除后台服务”取消服务的登录启动并保留本机数据。“登录时打开 App”单独控制托盘界面的登录启动。

macOS App 通过 SMAppService 注册包内 LaunchAgent，系统登录项会关联到 xrun App；需要授权时界面会提示前往系统设置。Windows 后台程序运行在当前用户的登录会话中，不显示控制台窗口。纯 CLI 安装继续使用原来的系统服务。

从 CLI 服务迁移时，先停止服务再在 App 中启动，App 会替换旧的服务注册；打开 App 本身不会中断已有任务。旧版 Windows daemon 不支持安全停止时，应先使用旧版 CLI 的 `xrun daemon uninstall`，再启动 App 服务。App 管理的 macOS 服务请在 App 中移除。

桌面端使用 Tauri 2 + React + TypeScript，Vite 构建前端，Bun 管理依赖、运行脚本和测试。本地构建需要 Rust、Bun 1.4.2，以及 macOS Xcode 命令行工具或 Windows MSVC / WebView2：

在项目根目录安装打包依赖：

```bash
bun install --cwd desktop --frozen-lockfile
```

然后在对应的平台执行打包命令：

| 平台 | 命令 | 默认产物位置（相对项目根目录） |
| --- | --- | --- |
| macOS | `bun run --cwd desktop build --bundles app` | `target/release/bundle/macos/xrun.app` |
| Windows | `bun run --cwd desktop build --bundles nsis` | `target/release/bundle/nsis/` 下的安装程序 |

本机调试时添加 `--debug`，产物改为 `target/debug/bundle/`：

```bash
bun run --cwd desktop build --debug --bundles app  # macOS
# Windows 使用 --bundles nsis
```

开发和检查同样可以在项目根目录运行：

```bash
bun run --cwd desktop dev       # 启动桌面端和前端热更新
bun run --cwd desktop check     # TypeScript 检查与界面测试
bun run --cwd desktop build:ui  # 只构建前端
```

macOS 开发模式可在界面中启动和停止后台 daemon，无需先打包 App；它复用本机身份与配置，退出开发界面后仍会运行，不设置登录启动。daemon 的运行日志写入 `~/.xrun/daemon-dev.log`。App 的登录启动需要使用打包后的 `xrun.app`。

桌面入口为 `desktop/scripts/desktop.ts`，先编译配套 daemon，再调用 Tauri；打包时自动构建前端，两者完整版本必须一致。默认构建目录是项目根目录的 `target/`；如果设置了 `CARGO_TARGET_DIR`，构建缓存和产物会使用指定目录。前端源代码在 `desktop/src/`，构建输出在 `desktop/dist/`。macOS 本地包默认对整个 App 和内嵌程序做 ad-hoc 签名，打包后校验签名；正式发布需要 Developer ID 签名和公证，可通过 `APPLE_SIGNING_IDENTITY` 指定签名身份。Windows 安装包目前未签名。

## 部署和加入

在 Linux Server 上执行：

```bash
xrun up --addr 192.168.1.10:9528
```

替换为其他设备可访问的 IPv4 地址或有 A 记录的域名，并放行 TCP 9528。省略 `--addr` 时自动探测地址；手动地址持久保留，重复 `up` 不会覆盖。Server 主机注册为管理设备 `admin`，默认同时启动本机 daemon。

`up` 默认安装 systemd 用户服务并启用 linger。systemd 是 Linux 自带的后台程序管理器，负责启动 Server 和 daemon，并在异常退出后重启；linger 让服务在退出 SSH 后继续运行，并在开机后启动。没有 systemd 时使用前台模式：

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

邀请有效期 10 分钟，只能使用一次，默认只注册设备。要允许 win1 控制 Server 主机，在 Server 主机执行 `xrun allow-from win1`；要允许 admin 控制 win1，在 win1 执行 `xrun allow-from admin`。

生成新邀请用 `xrun invite`。需要邀请双方互相授权时，使用 `xrun invite --allow` 或部署时使用 `xrun up --allow`；邀请方 daemon 离线时，在邀请方补执行 `xrun allow-from <新设备>`。邀请链接应通过可信渠道传递，带 `--allow` 的链接等于邀请方当前用户的命令执行权。撤销邀请方后，它未使用的邀请立即失效。

`join` 安装当前用户的 daemon：Linux 使用 systemd，macOS 使用 LaunchAgent，Windows 使用登录计划任务。只安装 CLI 身份可加 `--no-daemon`，之后用 `xrun daemon install` 安装服务，或 `xrun daemon` 前台运行。

LaunchAgent 是 macOS 的用户后台程序配置，由系统的 launchd 管理。本地 daemon 在登录后启动，异常退出后重启；关闭终端不影响它运行。`up` 和 `join` 会自动配置对应服务。

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

执行不会隐式启动 Shell。脚本从 stdin 读取，Shell 名称必填，支持 sh、bash、zsh、powershell、pwsh 和 cmd。PowerShell 脚本使用 UTF-8，cmd 脚本仅接受 ASCII，脚本参数不接受双引号或换行：

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

pull 和截图保存到指定本地文件时，拒绝目标文件的符号链接；保存目录需由调用方信任。远端 push 仍跟随符号链接，写入链接指向的文件。

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

CLI 安装的服务可用 `xrun daemon stop` 正常停止、`xrun daemon start` 再启动。停止也会取消本机运行中的任务。

## 断线与结果确认

前台执行原样输出远端 stdout/stderr，并保留远端退出码。超时为 124，本地参数错误为 2，连接、身份或执行层错误为 125；已确认的文件或截图操作失败为 1。日志不完整会注明原因。

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

已在 macOS、Linux 和 Windows 原生环境运行 TLS 配对、执行、后台任务、去重、并发、文件传输、撤销与故障恢复测试，并在 Linux Xvfb 中验证 PNG 截图。[Test Linux](.github/workflows/test-linux.yml)、[Test macOS](.github/workflows/test-macos.yml) 和 [Test Windows](.github/workflows/test-windows.yml) 分别在对应平台原生运行测试。测试直接调用 Server 库以覆盖转发，公开的 Server CLI 仍仅支持 Linux。

Ubuntu 26.04 x86_64 云主机与 macOS ARM64 本机已通过公网 TCP 8080 实机检查：双向执行与文件传输、任务去重/等待/取消、授权、繁忙重试、Server 中断后的日志恢复，以及 systemd 和 LaunchAgent 的异常重启。云端无桌面时截图返回 `NO_DISPLAY` 和退出码 1。

系统服务、真实桌面截图权限/锁屏、设备睡眠和磁盘故障的验收进度见 [design.md](design.md)。
