# xrun 使用手册

本手册介绍安装、加入、授权和日常使用。`xrun help` 提供速查，`xrun help push` 或 `xrun linux1 push --help` 查看单个命令的参数与短例子。

`xrun doc` 离线输出本手册，`xrun doc --list` 列出章节，`xrun doc access` 等命令查看单章。帮助与文档不需要身份、网络或运行中的 daemon，直接输出文本，可重定向或交给 AI 阅读。

| 章节 | 内容 |
| --- | --- |
| `install` | 安装 |
| `quickstart` | 开始使用 |
| `access` | 访问权限 |
| `execute` | 执行程序 |
| `jobs` | 任务与日志 |
| `files` | 文件与截图 |
| `streaming` | 流式执行 |
| `forward` | 端口转发 |
| `desktop` | 桌面 App |
| `relay` | 中转部署 |
| `config` | 配置与服务 |
| `errors` | 状态与排错 |
| `upgrade` | 升级与移除 |

## 安装

CLI 和 daemon 是同一个 Rust 二进制，运行已构建程序不需要 Rust、Node.js 或 Bun。桌面 App 管理网络、授权与服务，远程执行和文件传输使用 CLI。所有组件必须使用相同的完整发布版本，当前为 `0.0.1-beta.3`。

| 平台 | 安装方式 |
| --- | --- |
| macOS Apple Silicon | macOS 13+，DMG 拖入应用程序，或完整 App ZIP / CLI 压缩包 |
| Windows x86_64 | 当前用户 NSIS 安装程序，或 CLI 压缩包 |
| Linux x86_64 | CLI 压缩包，不提供桌面 App |

CLI 可放在 `~/.local/bin/xrun`，Windows 可用 `%LOCALAPPDATA%\xrun\bin\xrun.exe`。将工具目录加入 PATH，运行 `xrun --version`、`xrun help`。服务注册当前二进制的绝对路径，请安装到固定位置。

### AI 和命令行自动安装

[install.sh](../install.sh) 支持 macOS Apple Silicon，[install.ps1](../install.ps1) 支持 Windows x86_64。指定完整发布版本，默认安装桌面 App；`--component cli`／`-Component cli` 只安装 CLI。脚本校验平台、版本、SHA-256 和安装后的程序；macOS App 另检查签名。整个过程通过 stdout 返回一条 JSON，失败时 `ok` 为 `false`、包含 `error.code` 和 `error.message`，进程退出码为 1；成功返回版本、安装路径、可执行文件路径、`changed` 和自检结果。重复安装相同产物时返回 `changed: false`。

先下载发布附件中的安装脚本，再执行：

```bash
bash install.sh --version 0.0.1-beta.3
bash install.sh --version 0.0.1-beta.3 --component cli
```

```powershell
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File .\install.ps1 -Version 0.0.1-beta.3
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File .\install.ps1 -Version 0.0.1-beta.3 -Component cli
```

默认下载地址是 `https://github.com/qczone/xrun/releases/download/v<完整版本>/`。`--base-url`／`-BaseUrl` 可指定其他 HTTPS 产物目录。每个目录需包含对应的 `xrun-darwin-arm64.json` 或 `xrun-windows-x86_64.json` 清单及其引用的文件。安装时不需要 Bun；产物准备见 [开发与发布](development.md#发布与内置文档)。

离线安装或验收本地打包产物时，用 `--source-dir`／`-SourceDir` 指定产物目录。例如 macOS：

```bash
bash install.sh --version 0.0.1-beta.3 --source-dir ./dist
```

默认 macOS App 安装到 `~/Applications/xrun.app`，CLI 安装到 `~/.local/bin/xrun`；Windows App 安装到 `%LOCALAPPDATA%\Programs\xrun`，CLI 安装到 `%LOCALAPPDATA%\xrun\bin`。`--install-dir`／`-InstallDir` 可指定父目录，AI 可直接使用 JSON 返回的 `executable` 路径调用程序。脚本只安装程序；网络加入、设备授权和后台服务的启用继续使用现有 App 或 CLI 命令。

macOS 更新前需关闭已安装的 App，脚本会正常停止旧 daemon，再替换文件；CLI 更新也会先停止 daemon。相同产物的重复安装不停止服务。macOS App 和 CLI、Windows CLI 的替换或自检失败会恢复旧文件。Windows App 使用 NSIS 静默安装；服务准备失败时不会弹窗或替换程序，返回 32（更新）或 33（卸载），诊断保存在安装目录的 `xrun-install-error.log`。

## 开始使用

先准备所有设备可访问的中转，见 `xrun doc relay`。示例中 mac1 是管理设备，linux1 是加入的被控设备。部署中转本身不创建管理设备身份。

在 Mac 创建网络：

```bash
xrun up --relay '<完整中转地址或 xrun-relay:// 部署链接>' --name mac1
```

Mac 成为管理设备，准备身份与任务库、安装并启动 daemon，输出 `xrun://` 成员邀请。已有网络在管理设备运行 `xrun invite` 生成新邀请。

在 Linux 加入并允许 Mac 访问：

```bash
xrun join '<完整的 xrun:// 成员邀请>' --name linux1
xrun allow-from mac1
xrun status
```

邀请 10 分钟内单次使用，加入时管理设备 daemon 必须在线。普通邀请只注册成员；`allow-from` 在被控的 Linux 执行。

回到 Mac 验证：

```bash
xrun linux1 info
xrun linux1 -- uname -s
```

预期输出 Linux。`SOURCE_NOT_ALLOWED` 表示应在 Linux 允许 mac1。需要反向控制 Mac 时，在 Mac 执行 `xrun allow-from linux1`。

已有成员日常执行不要求管理设备在线，需要中转、两端在线且证书有效。没有可用 systemd 用户服务的 Linux 容器，用 `--no-daemon`，随后在单独终端或环境进程管理器下运行 daemon：

```bash
xrun join '<完整成员邀请>' --name linux1 --no-daemon
xrun allow-from mac1
xrun daemon
```

最后一条保持前台运行。`up --no-daemon` 同样只准备身份和数据库；管理设备必须之后运行 daemon 才能接收加入。

## 访问权限

管理设备签发成员身份，被控设备决定访问授权，中转接通连接并转发密文。成员身份不自动授予执行、文件、截图、任务查询或转发权限。

`xrun-relay://` 部署链接或 Cloudflare HTTPS 地址用于 `up --relay`，可重复使用；`xrun://` 成员邀请用于 `join`，10 分钟内单次使用。完整链接应通过可信渠道传递。

在被控设备执行，例如 Linux 允许或拒绝 Mac：

```bash
xrun allow-from mac1
xrun deny-from mac1
```

只允许管理设备生成邀请和撤销成员：`xrun invite`、`xrun revoke win1`。双方需要互相授权时，用 `xrun invite --allow` 或 `xrun up --allow`；否则分别执行 `xrun allow-from <来源设备>`。带 `--allow` 的链接等于管理设备当前用户的命令执行权，应通过可信渠道传递。普通成员之间各自授权。

撤销使用管理设备签名的成员清单，通过设备间加密连接同步，并报告未确认收到的设备；已收到记录的设备拒绝被撤销来源。离线设备或被中转隔离的设备可能尚未收到记录，紧急阻断在目标本机执行 `deny-from` 或暂停访问。已受理的可靠任务继续按任务语义处理。

全体信任、暂停和恢复同样在被控设备操作：

```bash
xrun allow-from --all
xrun deny-from --all
xrun daemon pause
xrun daemon resume
```

设备名是别名，认证、白名单和去重使用不可变设备 ID；命令也接受 `dev_…`。daemon 以当前用户权限运行。`allow-from --all` 包含当前和未来加入的未撤销成员，单独拒绝优先；`deny-from --all` 只关闭全体信任，保留单独允许和拒绝记录。

`deny-from` 拒绝指定来源；`pause` 暂停本机远程访问并使已有业务会话失效，恢复后按原授权重新建立会话。两者都保留已受理的可靠任务，但会清理流式进程和转发连接。`revoke` 则撤销网络成员身份，在收到签名更新的设备上生效，不取消已有可靠任务或回滚副作用。

## 执行程序

在调用设备指定目标、远端工作目录、程序和参数：

```bash
xrun linux1 -C /home/user/demo -- cargo test
xrun win1 -C 'D:\demo' -- cargo build
xrun linux1 --env MODE=test --env RUST_LOG=info -C /home/user/demo -- ./demo
```

`--` 分隔 xrun 选项和远端程序参数。目标 daemon 使用自己的 PATH 查找程序，调用设备的 PATH 不会自动传过去；必要时用完整路径。Shell 的管道、变量展开、重定向需要显式选择 Shell，否则由调用设备的 Shell 解释。

前台可靠执行在提交命令的同一会话中接收日志和最终结果。本机运行 daemon 时，CLI 还会自动复用已完成请求的加密连接，连续调用同一设备可省去重复建连；每次复用仍检查授权。每台本机最多保留两条空闲连接，每个目标一条，空闲 30 秒后关闭；缓存连接占用现有中转会话名额。本机未运行 daemon 时仍可直接发起调用。

执行默认超时 30 分钟，`start` 默认不限时；用 `--timeout 秒` 覆盖，0 表示不限时。`wait --timeout 秒` 只限制等待，不取消任务。`-C` 必须是远端绝对目录，省略时使用 daemon 默认目录。`--env KEY=VALUE` 可重复指定；`--stdin` 提交前读完输入，最多 1 MiB。

执行不会隐式启动 Shell。脚本从 stdin 读取，Shell 名称必填，支持 sh、bash、zsh、powershell、pwsh 和 cmd。PowerShell 脚本使用 UTF-8；cmd 脚本仅接受 ASCII，其脚本参数不接受双引号或换行：

```bash
xrun linux1 -C /repo --script bash < ./build.sh
xrun win1 -C 'D:\demo' --script powershell < ./build.ps1
```

可靠输入示例：

```bash
printf '%s\n' 'hello' | xrun linux1 --stdin -- cat
```

普通执行属于可靠任务；受理后任务和结果在目标保存。需要立即返回用 `start`，需要与连接一同结束用 `-i`。

## 任务与日志

在调用设备启动后台任务：

```bash
xrun linux1 start -C /home/user/demo -- ./server
```

返回包含目标和六字符任务 ID 的任务引用。以下 ABC123 替换为实际返回值，也可使用完整任务引用，无需预生成 UUID。

```bash
xrun linux1 jobs --running
xrun linux1 jobs ABC123 --json
xrun linux1 logs ABC123 --tail 40
xrun linux1 logs ABC123 --follow
xrun linux1 wait ABC123 --timeout 60
xrun linux1 kill ABC123
```

`jobs` 查询目标设备上由本机身份提交的可靠任务，默认每页 50 条，可用 `--limit`、`--offset` 分页；`recent` 只列本机最近 24 小时的提交记录，不代表远端任务当前状态。App 的「活动记录」读取本机作为目标执行的记录。

`jobs` 指定 ID 时不能同时用 `--running` 或 `--request-id` 列表过滤条件。`logs` 默认读快照，`--follow` 补读后跟随，`--after N` 从日志序号之后读，`--tail N` 取末尾行；`--after` 与 `--tail` 互斥。缺失、截断、过期或不完整日志会提示。

`wait` 默认不限等待时间，显示最后 40 行；`--timeout` 只结束等待，不取消任务，未确认结果返回 75。`kill` 请求取消并等待确认。可靠任务不随 CLI / 中转断线结束；daemon 正常停止会取消本机任务，崩溃重启后未完成任务标记 lost，不自动重跑。

任务查询、日志和取消只操作当前来源提交的任务，不授予其他来源的任务控制权。结果恢复见状态与排错章节。

## 文件与截图

在调用设备执行，路径按源、目标排列：

```bash
xrun linux1 pull /home/user/demo/config.json ./config.json --json
# 本地编辑后上传
xrun linux1 push ./config.json /home/user/demo/config.json
xrun linux1 push ./result.txt /home/user/demo/output/result.txt --mkdir
xrun win1 screenshot ./screen.png --json
```

`--mkdir` 创建远端目标父目录。相对远端路径在目标默认目录解析，或用 `-C` 指定远端绝对目录；本地路径以调用设备当前目录为准。

push、pull 分块读写单个文件，最多 64 MiB，不把整个文件装入内存，不转换 BOM 或换行。保留 SHA-256 校验，接收完整且校验通过后才保存目标文件或输出到 stdout。push 两条路径必填；pull 省略本地目标时保存到唯一临时文件。`-` 表示本地 stdin/stdout：

```bash
xrun linux1 pull /repo/config.json -
printf '%s\n' 'hello' | xrun linux1 push - /repo/note.txt
xrun linux1 push ./config.json /repo/config.json --expect '<pull返回的sha256>'
```

pull 和截图保存到指定本地文件时，拒绝目标文件的符号链接；保存目录需由调用方信任。远端 push 仍跟随符号链接，写入链接指向的文件。

`--expect` 可防止覆盖修改后的目标；`--no-overwrite` 要求目标不存在，两者互斥。截图保存 PNG，省略目标路径时使用唯一临时文件。Linux 截图仅支持 X11；macOS 需要屏幕录制权限，Windows 需要可访问的交互桌面。网页截图可以在无桌面的设备上通过项目的无头浏览器脚本生成，再用 pull 取回。

网页截图使用项目现有脚本并下载图片：

```bash
xrun linux1 -C /home/user/demo -- node scripts/screenshot.js
xrun linux1 pull /home/user/demo/artifacts/page.png ./page.png
```

目录查询、编辑和 Git 同步使用现有工具。

## 流式执行

`-i` 实时传递 stdin、stdout 和 stderr，适合大输入或与本地管道组合。输入没有可靠任务的 1 MiB 总量限制，也不受单文件 64 MiB 上限限制：

```bash
xrun linux1 -i -- cat < ./large.bin > ./copy.bin
xrun linux1 -i -C /repo --timeout 600 -- tar -cf - artifacts > ./artifacts.tar
```

这是非终端流式执行，不提供 PTY 或交互 Shell。stdin 结束后仍可接收输出；Ctrl+C 或连接断开会清理远端受管理进程，不自动重跑。它不创建持久任务，不能通过 jobs/wait/logs/kill 恢复，也不提供文件摘要校验；与 `start`、`--json`、`--stdin`、`--script`、`--request-id` 互斥。需要断线后继续运行时使用普通执行或 `start`。

## 端口转发

`forward` 将远端回环端口映射到本机回环端口，例如访问远端的开发服务：

```bash
xrun linux1 forward 8080:3000
# 或让系统分配本地端口，并输出 JSON 中的 local_address
xrun linux1 forward 0:3000 --json
```

第一个例子在本机监听 `127.0.0.1:8080`，每条连接接到 linux1 的 `127.0.0.1:3000`，必要时尝试远端 `::1`。省略本地端口时使用相同端口，例如 `forward 3000`。命令保持前台运行，Ctrl+C 结束监听及已有连接；中断的连接不恢复，后续新连接重新接通。转发需要目标授权，每条 TCP 连接独占一个中转会话，仍受中转并发上限约束。

## 桌面 App

macOS 13 及以上打开 DMG，将 `xrun.app` 拖入“应用程序”，再从 `/Applications` 启动；Windows 运行安装程序，安装到当前用户的目录。打开 App 后可以用中转地址创建网络，或粘贴成员邀请加入网络，也可以查看连接状态、控制后台服务，以及允许其他设备访问本机。已有 CLI 身份和配置会直接复用。

App 分为本机、设备、活动记录、设置四个页面。本机概览分别显示后台服务、中转连接和远程访问状态；设备页管理其他设备对本机的访问，管理设备可通过页头入口生成邀请。「活动记录」展示在本机执行的任务，可以按状态筛选，查看来源设备、命令、工作目录、耗时、退出结果和 stdout/stderr；宽窗口并排显示列表与详情。运行中的输出每 3 秒刷新，向上滚动会暂停自动滚动，可点击“回到最新输出”继续跟随。其中「文件与截图」标签展示 push、pull 和 screenshot 的已有操作记录。App 只读本机 daemon 数据库，服务停止或网络断开后仍可查询，不会查询其他设备上执行的任务。

设置中可以选择默认工作目录、修改同时运行的任务数和工具搜索路径 PATH；保存后对新任务生效，无需重启。其他环境变量和已有权限保留。

已加入时，启动 App 默认显示菜单栏或系统托盘图标；点击图标打开管理窗口。关闭窗口会留在托盘。“隐藏图标”同时隐藏窗口，重新打开 App 会恢复图标和窗口。App 只有一个实例，重新打开不会启动第二个 daemon。

“退出 App”只退出界面。后台服务独立运行，随用户登录启动；“停止服务”正常结束 daemon 和本机运行中的任务，保留任务结果，服务会在下次登录时再启动。“移除后台服务”取消服务的登录启动并保留本机数据。“登录时显示 xrun 图标”单独控制托盘界面的登录启动。

macOS App 通过 SMAppService 注册包内 LaunchAgent，系统登录项会关联到 xrun App；需要授权时界面会提示前往系统设置。Windows 后台程序运行在当前用户的登录会话中，不显示控制台窗口。纯 CLI 安装继续使用原来的系统服务。

从 CLI 服务迁移时，先停止服务再在 App 中启动，App 会替换旧的服务注册；打开 App 本身不会中断已有任务。旧版 Windows daemon 不支持安全停止时，应先使用旧版 CLI 的 `xrun daemon uninstall`，再启动 App 服务。App 管理的 macOS 服务请在 App 中移除。

## 中转部署

中转可以部署到 Cloudflare，或自建 Linux 主机。Cloudflare 需要本机已登录 Wrangler：

```bash
bun install --cwd cloudflare --frozen-lockfile
bun run --cwd cloudflare deploy --name xrun-relay
```

命令输出完整 HTTPS 地址；重复部署同名 Worker 复用随机路由。保留 `cloudflare/.deploy/` 中的私有部署状态，勿提交到 Git。把完整地址粘贴到 App，或创建网络：

```bash
xrun up --relay 'https://<Worker 域名>/<随机路由>' --name mac1
```

Cloudflare 使用公共 HTTPS 证书，设备间仍使用独立的网络根证书和双向 TLS。中转不保存成员名单、任务、日志或文件，只保留在线连接和临时转发状态。单文件支持 64 MiB；每方向 4 MiB 的传输窗口限制尚未确认的密文，每个网络最多同时保留 8 个中转会话。部署、检查和删除说明见 [cloudflare/README.md](../cloudflare/README.md)。

如果使用自建 Linux 中转，在 Linux 主机上执行：

```bash
xrun relay install --addr 192.168.1.10:9528
```

替换为其他设备可访问的 IPv4 地址或有 A 记录的域名，并放行 TCP 9528。省略 `--addr` 时自动探测地址；手动地址持久保留。此命令安装 systemd 用户服务并启用 linger，负责开机启动和异常重启中转。中转只接通连接、转发设备间的 TLS 密文，网络管理密钥不保存在中转。

部署链接包含中转指纹和固定随机路由，可以重复使用，重启后保留。在中转主机可查看链接、前台运行已配置中转或移除服务：

```bash
xrun relay invite
xrun relay run
xrun relay uninstall
```

复制完整部署链接到管理设备运行 `up --relay` 创建网络。部署链接与 `join` 使用的成员邀请不同。无 systemd 的环境不能直接使用 `relay install` 完成服务安装，应使用有 systemd 的主机或 Cloudflare。

Cloudflare 多账号时设置 `CLOUDFLARE_ACCOUNT_ID`。删除自己部署且本地有记录的中转：

```bash
bun run --cwd cloudflare remove --name xrun-relay
```

删除后再次部署换一个名称。部署脚本需要源码和 Bun，日常 CLI 不需要 Bun。

## 配置与服务

`up` / `join` 默认安装服务：Linux systemd 用户服务和 linger，macOS 纯 CLI LaunchAgent，Windows 当前用户登录计划任务。daemon 以当前用户运行，默认同时最多 4 个任务。App 服务管理见桌面 App 章节。

Linux 容器缺少 `systemctl` / `loginctl` 或可用用户会话时，使用 `--no-daemon`，交由外部管理器运行 `xrun daemon`。`daemon_installed` 反映注册文件或标记存在，不能单独证明服务管理器正常，结合 `daemon_running` 和 `daemon_connected` 判断。

```bash
xrun daemon install
xrun daemon start
xrun daemon stop
xrun daemon uninstall
xrun daemon
```

`daemon install` 需要已准备好的设备身份；`daemon start` 启动已有服务；`daemon stop` 正常停止并取消本机运行中的任务；`daemon uninstall` 移除注册并保留数据。`daemon` 前台运行，关闭终端是否结束取决于外部管理方式。

配置、身份和数据库在 `~/.xrun`（Windows 为 `%USERPROFILE%\.xrun`）。工具链 PATH、默认目录和来源白名单写入 `daemon.toml`：

```toml
allow_from = ["dev_来源设备ID"]
default_cwd = "/path/to/repo"
max_concurrent_jobs = 4

[env]
PATH = "/home/user/.cargo/bin:/home/user/.local/bin:/usr/local/bin:/usr/bin:/bin"
```

远端命令保留符号链接入口，支持 rustup 的 cargo、rustc 等按启动名称分派的工具。任务继承 daemon 的正常环境，但过滤隐式的 `CARGO_TARGET_DIR`、`CARGO_BUILD_TARGET`、`RUSTUP_TOOLCHAIN`、`RUST_RECURSION_COUNT`、`RUSTC`、`RUSTDOC`、`RUSTC_WRAPPER`、`RUSTC_WORKSPACE_WRAPPER`、`RUSTFLAGS`、`CARGO_ENCODED_RUSTFLAGS`；工具目录、PATH 和代理等配置保留。需要指定构建目录、工具链或编译参数时，写入 `[env]` 或通过 `--env` 显式传入，请求值优先。开发版和发布版采用相同规则。

配置热更新对新任务生效。修改原文件中的所需字段，保留其他环境、允许 / 拒绝记录与暂停状态。PATH 使用目标系统格式，Windows 用分号分隔；确认目标工具目录如 `~/.cargo/bin`、`~/.local/bin` 在 PATH 中。上面的 `/home/user` 应替换为目标用户的实际主目录，配置中的 `~` 不会自动展开。

服务输出与任务 `logs` 不同。macOS 纯 CLI 服务日志在 `~/.xrun/daemon-service.log`；Linux 可用 `journalctl --user -u xrun-daemon.service` 查看；无 systemd 使用外部管理器日志或启动时重定向输出。

## 状态与排错

`status` 读取中转报告的连接列表，并按本机已验证的成员清单过滤；“已连接”不保证目标能执行命令，也不表示已同步最新成员变更。查看主机名、系统、版本等详情时，用 `xrun <设备> info` 单独连接目标验证。撤销送达仍以端点签名回执为准。

```bash
xrun status --json
xrun linux1 info --json
xrun recent --json
```

当前没有持久保存离线时间历史。`info` 的 `last_seen` 是目标响应查询时的时间，离线后不保留。Rust 中转每 15 秒检查心跳，突然断网通常约 45–60 秒后识别。

多数查询 / 操作的 `--json` 数据写到 stdout，错误诊断写到 stderr；普通前台执行仍传递远端 stdout / stderr。帮助、文档和参数解析错误保持文本。

前台执行原样输出远端 stdout/stderr，并保留远端退出码。超时为 124，本地参数错误为 2，连接、身份或执行层错误为 125；已确认的文件或截图操作失败为 1。日志不完整会注明原因。

返回 **75 表示结果未确认**。已知任务 ID 就查询 `jobs` 或 `wait`；只有请求 ID 时：

```bash
xrun recent
xrun win1 jobs --request-id '<request_id>'
```

恢复同一次提交时提供原参数、原输入和原 `--request-id`。daemon 按来源设备与请求 ID 去重，不重新运行已受理任务；数据库重建后返回 `DB_RESET`。普通调用会生成新请求 ID。

可靠执行的 CLI 或中转连接断开时任务继续运行，前台 CLI 会尝试重连并按日志序号补读，最多恢复 30 秒；仍无法确认结果时返回 75。Ctrl+C 会尝试取消可靠任务，无法确认取消时也返回 75；SIGTERM、SIGHUP 不取消已提交任务。daemon 正常停止会取消任务，崩溃后重启则将未完成任务标记为 lost，不重跑。流式执行和转发随连接结束清理。

`push` 响应丢失时不自动重发，先 `pull` 确认目标内容。连接缓存和重连不会自动重放已经提交的执行、上传或取消请求。

| 诊断 | 检查与处理 |
| --- | --- |
| SOURCE_NOT_ALLOWED | 在被控设备 allow-from 来源 |
| ACCESS_PAUSED | 在被控设备 daemon resume |
| DEVICE_OFFLINE / 连接失败 | 检查目标 daemon、中转和网络，不排队执行 |
| VERSION_MISMATCH | 中转、CLI、daemon 和 App helper 成套使用相同完整版本 |
| INVALID_SIGNATURE | 检查旧版本成员记录或损坏数据；当前没有跨版本签名迁移 |
| PROGRAM_NOT_FOUND | 检查目标 PATH，或用程序完整路径 |
| INVALID_CWD | -C 使用目标存在的绝对目录 |
| NO_DISPLAY | 目标需要可访问桌面；网页可用无头浏览器 |
| DB_RESET | 原任务库已重建，不能重放或推断原任务 |

确认取消或提交前 Ctrl+C 返回 130，远端被信号结束返回 128 + 信号。wait 超时 / 断线且不能确认已有任务结果时返回 75，不取消任务。业务程序也可以返回相同数值，要结合诊断、错误码与任务状态判断。

## 升级与移除

所有参与调用的 CLI、daemon、App helper 和中转检查完整版本，不进行版本协商。同一版本号的开发部署也应来自同一源码，不混用不同协议。

同一完整版本、同一协议构建的程序替换可以复用原身份和数据库：等待任务结束、停止服务，替换固定位置程序，启动并检查。

```bash
xrun daemon stop
# 替换程序或运行匹配版本的安装脚本
xrun --version
xrun daemon start
xrun status
```

跨版本需单独处理数据兼容。持久成员清单、配对回执等记录的签名包含完整发布版本，没有签名迁移或旧协议兼容层；直接保留旧记录可能报 INVALID_SIGNATURE。beta.2 → beta.3 需要重新创建网络、加入并授权。清理数据会删除身份、任务与日志，应在明确不需要这些数据后进行，不能当作普通重启。

移除本机服务保留数据使用 `down`，清理本机数据使用 `down --purge`，需要交互终端确认：

```bash
xrun down
xrun down --purge
```

macOS App 管理的服务在 App 中移除。Linux `down` 同时移除本机中转服务。`daemon reset` 要求 daemon 已停止，只重建任务数据库并保留设备身份；旧任务引用不能用于新库。
