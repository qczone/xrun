# xrun

xrun 让开发者和 AI 在已授权的 macOS、Windows、Linux 设备上执行程序、管理后台任务、传输单个文件、转发开发服务端口和截图。设备主动连接自建的 Linux Rust 中转或 Cloudflare 中转，无需向客户端开放入站端口。

创建网络的设备负责签发成员身份；目标 daemon 验证来源并决定授权。中转验证连接凭证、接通会话并转发端到端加密的数据，不保存成员清单或业务内容。任务、日志和去重记录保存在目标 daemon；代码同步使用 Git，文件在本地用自己的编辑工具修改。

## 构建

先安装 rustup。项目通过 [rust-toolchain.toml](rust-toolchain.toml) 固定 Rust 版本及 rustfmt、clippy 组件，本地构建、三平台 CI 和打包使用同一工具链；在项目目录运行 Cargo 时由 rustup 自动选择。

```bash
cargo build --locked --release
```

将 `target/release/xrun`（Windows 为 `xrun.exe`）放到固定目录并加入 PATH，再安装服务。CLI、daemon 和 Linux Rust 中转使用同一个二进制；Cloudflare 中转单独部署。参与调用的 CLI、daemon 和中转必须使用相同的完整发布版本。开发构建应使用同一份源码成套更新，发布要求见 [design.md 的版本说明](design.md#63-版本和限制)。

GitHub Actions 的 `Package` 工作流可手动构建三个平台的 CLI 压缩包，以及 macOS Apple Silicon DMG 和 Windows x86_64 用户级 NSIS 安装包；macOS App 和 DMG 的签名、公证需要配置工作流列出的凭据。

## 桌面 App

macOS 13 及以上打开 DMG，将 `xrun.app` 拖入“应用程序”，再从 `/Applications` 启动；Windows 运行安装程序，安装到当前用户的目录。打开 App 后可以用中转地址创建网络，或粘贴成员邀请加入网络，也可以查看连接状态、控制后台服务，以及允许其他设备访问本机。已有 CLI 身份和配置会直接复用。

App 分为本机、设备、活动记录、设置四个页面。本机概览分别显示后台服务、中转连接和远程访问状态；设备页管理其他设备对本机的访问，管理设备可通过页头入口生成邀请。「活动记录」展示在本机执行的任务，可以按状态筛选，查看来源设备、命令、工作目录、耗时、退出结果和 stdout/stderr；宽窗口并排显示列表与详情。运行中的输出每 3 秒刷新，向上滚动会暂停自动滚动，可点击“回到最新输出”继续跟随。其中「文件与截图」标签展示 push、pull 和 screenshot 的已有操作记录。App 只读本机 daemon 数据库，服务停止或网络断开后仍可查询，不会查询其他设备上执行的任务。

设置中可以选择默认工作目录、修改同时运行的任务数和工具搜索路径 PATH；保存后对新任务生效，无需重启。其他环境变量和已有权限保留。

已加入时，启动 App 默认显示菜单栏或系统托盘图标；点击图标打开管理窗口。关闭窗口会留在托盘。“隐藏图标”同时隐藏窗口，重新打开 App 会恢复图标和窗口。App 只有一个实例，重新打开不会启动第二个 daemon。

“退出 App”只退出界面。后台服务独立运行，随用户登录启动；“停止服务”正常结束 daemon 和本机运行中的任务，保留任务结果，服务会在下次登录时再启动。“移除后台服务”取消服务的登录启动并保留本机数据。“登录时显示 xrun 图标”单独控制托盘界面的登录启动。

macOS App 通过 SMAppService 注册包内 LaunchAgent，系统登录项会关联到 xrun App；需要授权时界面会提示前往系统设置。Windows 后台程序运行在当前用户的登录会话中，不显示控制台窗口。纯 CLI 安装继续使用原来的系统服务。

从 CLI 服务迁移时，先停止服务再在 App 中启动，App 会替换旧的服务注册；打开 App 本身不会中断已有任务。旧版 Windows daemon 不支持安全停止时，应先使用旧版 CLI 的 `xrun daemon uninstall`，再启动 App 服务。App 管理的 macOS 服务请在 App 中移除。

桌面端使用 Tauri 2 + React + TypeScript，Vite 构建前端，Bun 管理依赖、运行脚本和测试。本地构建需要 Rust、Bun 1.4.2，以及 macOS Xcode 命令行工具或 Windows MSVC / WebView2：

在项目根目录安装打包依赖：

```bash
bun install --cwd desktop --frozen-lockfile
```

然后在对应的平台执行同一条打包命令，自动生成本机的安装包：

```bash
bun run --cwd desktop build
```

| 平台 | 安装包 | 默认产物位置（相对项目根目录） |
| --- | --- | --- |
| macOS | DMG，拖入“应用程序”安装 | `target/release/bundle/dmg/` 下的 `.dmg` |
| Windows | 当前用户的 NSIS 安装程序 | `target/release/bundle/nsis/` 下的安装 `.exe` |

macOS 构建同时保留 `target/release/bundle/macos/xrun.app`，用于签名检查和调试；发布时分发 DMG。

本机调试时添加 `--debug`，产物改为 `target/debug/bundle/`：

```bash
bun run --cwd desktop build --debug
```

开发和检查同样可以在项目根目录运行：

```bash
bun run --cwd desktop dev       # 启动桌面端和前端热更新
bun run --cwd desktop check     # TypeScript 检查与界面测试
bun run --cwd desktop build:ui  # 只构建前端
```

macOS 开发模式可在界面中启动和停止后台 daemon，无需先打包 App；它复用本机身份与配置，退出开发界面后仍会运行，不设置登录启动。daemon 的运行日志写入 `~/.xrun/daemon-dev.log`。App 的登录启动需要使用打包后的 `xrun.app`。

桌面入口为 `desktop/scripts/desktop.ts`，先编译配套 daemon，再调用 Tauri；打包时自动构建前端，两者完整版本必须一致。平台配置 `tauri.macos.conf.json` 和 `tauri.windows.conf.json` 分别指定 DMG 和 NSIS。默认构建目录是项目根目录的 `target/`；如果设置了 `CARGO_TARGET_DIR`，构建缓存和产物会使用指定目录。前端源代码在 `desktop/src/`，构建输出在 `desktop/dist/`。macOS 本地包默认对整个 App 和内嵌程序做 ad-hoc 签名，打包后校验签名；正式发布需要 Developer ID 签名和公证，可通过 `APPLE_SIGNING_IDENTITY` 指定签名身份，GitHub Actions 会对包内 App 和最终 DMG 分别公证并附加公证票据。Windows 安装包目前未签名。

## 部署和加入

中转可以部署到 Cloudflare，或自建 Linux 主机。Cloudflare 需要本机已登录 Wrangler：

```bash
bun install --cwd cloudflare --frozen-lockfile
bun run --cwd cloudflare deploy --name xrun-relay
```

命令输出完整 HTTPS 地址；重复部署同名 Worker 复用随机路由。保留 `cloudflare/.deploy/` 中的私有部署状态，勿提交到 Git。把完整地址粘贴到 App，或创建网络：

```bash
xrun up --relay 'https://<Worker 域名>/<随机路由>' --name mac1
```

Cloudflare 使用公共 HTTPS 证书，设备间仍使用独立的网络根证书和双向 TLS。中转不保存成员名单、任务、日志或文件，只保留在线连接和临时转发状态。单文件支持 64 MiB；每方向 4 MiB 的传输窗口限制尚未确认的密文，每个网络最多同时保留 8 个中转会话。部署、检查和删除说明见 [cloudflare/README.md](cloudflare/README.md)。

如果使用自建 Linux 中转，在 Linux 主机上执行：

```bash
xrun relay install --addr 192.168.1.10:9528
```

替换为其他设备可访问的 IPv4 地址或有 A 记录的域名，并放行 TCP 9528。省略 `--addr` 时自动探测地址；手动地址持久保留。此命令安装 systemd 用户服务并启用 linger，负责开机启动和异常重启中转。中转只接通连接、转发设备间的 TLS 密文，网络管理密钥不保存在中转。

在 Mac、Windows 或 Linux 设备上，复制中转输出的完整部署链接创建网络：

```bash
xrun up --relay 'xrun-relay://192.168.1.10:9528/<中转指纹>#<随机路由>' --name mac1
```

这台设备成为唯一管理设备，保存网络根密钥和权威成员数据库。部署链接包含固定随机路由，可重复使用，重启后保留；在中转主机执行 `xrun relay invite` 可再次查看。完整链接需要保密；成员邀请里也含有这个路由，邀请过期后路由仍然有效。中转用网络 ID（网络根公钥指纹）验证每个连接出示的成员证书，不保存成员名单或执行记录：没有成员证书只能向管理设备发起配对，不能冒用其他设备上线、查询在线设备或连接其他设备。执行权限仍由端点验证。它与成员邀请链接不同。

在其他设备使用 `up` 输出的完整成员邀请链接，例如 Windows：

```bash
xrun join '<up 输出的 xrun:// 链接>' --name win1
xrun status
# 在 win1 上允许 mac1 访问本机
xrun allow-from mac1
```

邀请有效期 10 分钟、单次使用，默认只注册设备。加入时管理设备的 daemon 必须在线；Token 在核对管理设备 TLS 身份后发送，中转看不到它。已有成员之间的日常执行、文件传输和转发不要求管理设备在线，但仍需要中转、两端在线且证书有效。

只允许管理设备生成邀请和撤销成员：`xrun invite`、`xrun revoke win1`。双方需要互相授权时，用 `xrun invite --allow` 或 `xrun up --allow`；否则分别执行 `xrun allow-from <来源设备>`。带 `--allow` 的链接等于管理设备当前用户的命令执行权，应通过可信渠道传递。普通成员之间各自授权。

撤销使用管理设备签名的成员清单，通过设备间加密连接同步，并报告未确认收到的设备；已收到记录的设备拒绝被撤销来源。离线设备或被中转隔离的设备可能尚未收到记录，紧急阻断在目标本机执行 `deny-from` 或暂停访问。已受理的可靠任务继续按任务语义处理。

前台运行可用 `xrun relay run`、`xrun daemon`。`up --no-daemon` 和 `join --no-daemon` 准备身份、成员清单与任务数据库，不安装后台服务；创建网络后需要运行管理设备 daemon 才能接收加入请求。

升级程序前结束在途任务并停止服务，保留 `~/.xrun` 中的身份、成员清单、任务和配置，再成套更新程序并启动服务。当前网络的正常程序升级无需重新加入。管理状态丢失后需要重新组网，当前不提供管理身份恢复。

`join` 安装当前用户的 daemon：Linux 使用 systemd，macOS 使用 LaunchAgent，Windows 使用登录计划任务。使用 `--no-daemon` 后，可再用 `xrun daemon install` 安装服务，或 `xrun daemon` 前台运行。

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

前台可靠执行在提交命令的同一会话中接收日志和最终结果。本机运行 daemon 时，CLI 还会自动复用已完成请求的加密连接，连续调用同一设备可省去重复建连；每次复用仍检查授权。每台本机最多保留两条空闲连接，每个目标一条，空闲 30 秒后关闭；缓存连接占用现有中转会话名额。本机未运行 daemon 时仍可直接发起调用。

执行默认超时 30 分钟，`start` 默认不限时；用 `--timeout 秒` 覆盖，0 表示不限时。`wait --timeout 秒` 只限制等待，不取消任务。`-C` 必须是远端绝对目录，省略时使用 daemon 默认目录。`--env KEY=VALUE` 可重复指定；`--stdin` 提交前读完输入，最多 1 MiB。

执行不会隐式启动 Shell。脚本从 stdin 读取，Shell 名称必填，支持 sh、bash、zsh、powershell、pwsh 和 cmd。PowerShell 脚本使用 UTF-8；cmd 脚本仅接受 ASCII，其脚本参数不接受双引号或换行：

```bash
xrun linux1 -C /repo --script bash < ./build.sh
xrun win1 -C 'D:\demo' --script powershell < ./build.ps1
```

push、pull 分块读写单个文件，最多 64 MiB，不把整个文件装入内存，不转换 BOM 或换行。保留 SHA-256 校验，接收完整且校验通过后才保存目标文件或输出到 stdout。push 两条路径必填；pull 省略本地目标时保存到唯一临时文件。`-` 表示本地 stdin/stdout：

```bash
xrun linux1 pull /repo/config.json -
printf '%s\n' 'hello' | xrun linux1 push - /repo/note.txt
xrun linux1 push ./config.json /repo/config.json --expect '<pull返回的sha256>'
```

pull 和截图保存到指定本地文件时，拒绝目标文件的符号链接；保存目录需由调用方信任。远端 push 仍跟随符号链接，写入链接指向的文件。

`--expect` 可防止覆盖修改后的目标；`--no-overwrite` 要求目标不存在，两者互斥。截图保存 PNG，省略目标路径时使用唯一临时文件。Linux 截图仅支持 X11；macOS 需要屏幕录制权限，Windows 需要可访问的交互桌面。网页截图可以在无桌面的设备上通过项目的无头浏览器脚本生成，再用 pull 取回。

## 流式执行与端口转发

`-i` 实时传递 stdin、stdout 和 stderr，适合大输入或与本地管道组合。输入没有可靠任务的 1 MiB 总量限制，也不受单文件 64 MiB 上限限制：

```bash
xrun linux1 -i -- cat < ./large.bin > ./copy.bin
xrun linux1 -i -C /repo --timeout 600 -- tar -cf - artifacts > ./artifacts.tar
```

这是非终端流式执行，不提供 PTY 或交互 Shell。stdin 结束后仍可接收输出；Ctrl+C 或连接断开会清理远端受管理进程，不自动重跑。它不创建持久任务，不能通过 jobs/wait/logs/kill 恢复，也不提供文件摘要校验；与 `start`、`--json`、`--stdin`、`--script`、`--request-id` 互斥。需要断线后继续运行时使用普通执行或 `start`。

`forward` 将远端回环端口映射到本机回环端口，例如访问远端的开发服务：

```bash
xrun linux1 forward 8080:3000
# 或让系统分配本地端口，并输出 JSON 中的 local_address
xrun linux1 forward 0:3000 --json
```

第一个例子在本机监听 `127.0.0.1:8080`，每条连接接到 linux1 的 `127.0.0.1:3000`，必要时尝试远端 `::1`。省略本地端口时使用相同端口，例如 `forward 3000`。命令保持前台运行，Ctrl+C 结束监听及已有连接；中断的连接不恢复，后续新连接重新接通。转发需要目标授权，每条 TCP 连接独占一个中转会话，仍受中转并发上限约束。

## 授权、查询和停止

`status` 读取中转报告的连接列表，并按本机已验证的成员清单过滤；“已连接”不保证目标能执行命令，也不表示已同步最新成员变更。查看主机名、系统、版本等详情时，用 `xrun <设备> info` 单独连接目标验证。撤销送达仍以端点签名回执为准。

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

# 在本机开启或关闭对全部成员的信任
xrun allow-from --all
xrun deny-from --all

# 暂停或恢复本机的远程访问
xrun daemon pause
xrun daemon resume

# 仅管理设备可以撤销身份
xrun revoke win1

xrun daemon uninstall
xrun down
```

设备名是别名，认证、白名单和去重使用不可变设备 ID；命令也接受 `dev_…`。daemon 以当前用户权限运行。`allow-from --all` 包含当前和未来加入的未撤销成员，单独拒绝优先；`deny-from --all` 只关闭全体信任，保留单独允许和拒绝记录。

`deny-from` 拒绝指定来源；`pause` 暂停本机远程访问并使已有业务会话失效，恢复后按原授权重新建立会话。两者都保留已受理的可靠任务，但会清理流式进程和转发连接。`revoke` 则撤销网络成员身份，在收到签名更新的设备上生效，不取消已有可靠任务或回滚副作用。

`jobs` 查询目标设备上由本机身份提交的可靠任务，默认每页 50 条，可用 `--limit`、`--offset` 分页；`recent` 只列本机最近 24 小时的提交记录，不代表远端任务当前状态。App 的「活动记录」读取本机作为目标执行的记录。

配置、身份和数据库在 `~/.xrun`（Windows 为 `%USERPROFILE%\.xrun`）。工具链 PATH、默认目录和来源白名单写入 `daemon.toml`：

```toml
allow_from = ["dev_来源设备ID"]
default_cwd = "/path/to/repo"
max_concurrent_jobs = 4

[env]
PATH = "/usr/local/bin:/usr/bin:/bin"
```

远端命令保留符号链接入口，支持 rustup 的 cargo、rustc 等按启动名称分派的工具。任务继承 daemon 的正常环境，但过滤隐式的 `CARGO_TARGET_DIR`、`CARGO_BUILD_TARGET`、`RUSTUP_TOOLCHAIN`、`RUST_RECURSION_COUNT`、`RUSTC`、`RUSTDOC`、`RUSTC_WRAPPER`、`RUSTC_WORKSPACE_WRAPPER`、`RUSTFLAGS`、`CARGO_ENCODED_RUSTFLAGS`；工具目录、PATH 和代理等配置保留。需要指定构建目录、工具链或编译参数时，写入 `[env]` 或通过 `--env` 显式传入，请求值优先。开发版和发布版采用相同规则。

`xrun guide` 输出构建时嵌入的本 README；更新使用说明后需重新构建程序。

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

可靠执行的 CLI 或中转连接断开时任务继续运行，前台 CLI 会尝试重连并按日志序号补读，最多恢复 30 秒；仍无法确认结果时返回 75。Ctrl+C 会尝试取消可靠任务，无法确认取消时也返回 75；SIGTERM、SIGHUP 不取消已提交任务。daemon 正常停止会取消任务，崩溃后重启则将未完成任务标记为 lost，不重跑。流式执行和转发随连接结束清理。

push 响应丢失时不自动重发，先 pull 确认目标内容。连接缓存和重连不会自动重放已经提交的执行、上传或取消请求。

## 验证

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
# Linux X11 截图测试（需安装 Xvfb、libX11、libXrandr）
xvfb-run -a -s '-screen 0 1024x768x24' cargo test --locked --test screenshot -- --ignored
```

[Test Linux](.github/workflows/test-linux.yml)、[Test macOS](.github/workflows/test-macos.yml) 和 [Test Windows](.github/workflows/test-windows.yml) 分别在对应平台原生运行核心测试，覆盖配对、授权、执行、任务、文件传输、流式执行、端口转发、会话缓存和故障恢复。测试直接调用 Rust 中转库，`xrun relay` 部署命令仍仅支持 Linux。Linux 工作流另运行 Xvfb 截图测试和 Cloudflare workerd 测试；macOS、Windows 还检查桌面端和安装包内的 helper。每次提交是否通过，以对应的 Actions 结果为准。

Cloudflare 中转的本地检查无需云端凭证：

```bash
bun run --cwd cloudflare check
bun run --cwd cloudflare test
```

Ubuntu 26.04 x86_64 云主机与 macOS ARM64 本机已通过公网 TCP 8080 实机检查：双向执行与文件传输、任务去重/等待/取消、授权、繁忙重试、中转中断后的日志恢复，以及 systemd 和 LaunchAgent 的异常重启。另已通过真实 Cloudflare 中转验证独立 CLI 进程间的会话复用、64 MiB 文件往返、空闲恢复和撤销。云端无桌面时截图返回 `NO_DISPLAY` 和退出码 1。

系统服务、真实桌面截图权限/锁屏、设备睡眠和磁盘故障的验收进度见 [design.md](design.md)。
