# xrun 使用手册

xrun 在你自己的 macOS、Windows 和 Linux 设备之间执行程序、管理后台任务、传输文件、截图和转发端口。本手册介绍安装、组网、授权和日常使用。

`xrun help [命令]` 查看命令速查，`xrun doc` 离线输出本手册，`xrun doc <章节>` 只看一章，`xrun doc --list` 列出章节。帮助和手册不需要身份、网络或运行中的服务，可以直接交给 AI 阅读。AI 和脚本的调用流程见 `xrun doc automation`。

| 章节 | 内容 |
| --- | --- |
| `install` | 安装 |
| `quickstart` | 开始使用 |
| `relay` | 中转部署 |
| `access` | 访问权限 |
| `network` | 网络管理 |
| `execute` | 执行程序 |
| `jobs` | 任务与日志 |
| `automation` | 自动化与 AI |
| `files` | 文件与截图 |
| `streaming` | 流式执行 |
| `forward` | 端口转发 |
| `desktop` | 桌面 App |
| `config` | 配置与服务 |
| `errors` | 状态与排错 |
| `upgrade` | 升级与移除 |

## 安装

| 平台 | 下载文件 | 内容 |
| --- | --- | --- |
| macOS 13+ | `xrun-app-darwin-<架构>.zip` | 桌面 App，自带 CLI |
| Windows | `xrun-app-windows-<架构>.exe` | 当前用户安装程序，包含桌面 App 和 CLI |
| Linux | `xrun-linux-<架构>.tar.gz` | 只有 `xrun` 一个程序 |

架构为 `x86_64`（即 AMD64，Intel Mac 选这个）或 `arm64`（Apple Silicon、Windows ARM、ARM Linux）。从 [Release](https://github.com/xrun-sh/xrun/releases) 下载对应文件。CLI、后台服务（daemon）和 Linux 中转是同一个程序，运行时不需要 Rust、Node.js 或 Bun。

- **macOS**：解压得到 `xrun.app`，先移到 `/Applications` 或 `~/Applications` 再打开。首次打开会把 CLI 链接到 `~/.local/bin/xrun`，并为 zsh 和 Bash 配置 PATH，重新打开终端即可运行 `xrun --version`。直接在下载目录打开时，macOS 会从临时隔离位置运行 App，此时不会创建 CLI 入口。
- **Windows**：运行安装程序。它同时安装 App 和 CLI，并把安装目录下的 `cli` 加入当前用户 PATH；卸载时只移除它自己添加的 PATH 项。
- **Linux**：把压缩包里的 `xrun` 放到固定位置（例如 `~/.local/bin/`）并加入 PATH。后台服务注册的是程序的绝对路径，安装后不要随意移动。

### 命令行安装

每个 Release 附带 `install.sh`（macOS、Linux）和 `install.ps1`（Windows）。脚本自动识别架构，校验版本、SHA-256 和安装后的程序，macOS 另检查签名，适合 AI 和自动化使用。`<版本>` 替换为 Release 的完整版本号：

```bash
curl -fLO https://github.com/xrun-sh/xrun/releases/download/v<版本>/install.sh
bash install.sh --version <版本>
```

```powershell
Invoke-WebRequest https://github.com/xrun-sh/xrun/releases/download/v<版本>/install.ps1 -OutFile install.ps1
powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File .\install.ps1 -Version <版本>
```

| install.sh | install.ps1 | 作用 |
| --- | --- | --- |
| `--version` | `-Version` | 必填，完整发布版本 |
| `--install-dir` | `-InstallDir` | 安装父目录 |
| `--source-dir` | `-SourceDir` | 从本地产物目录离线安装 |
| `--base-url` | `-BaseUrl` | 从其他 HTTPS 产物目录下载 |
| `--arch x86_64` | — | 在装有 Rosetta 的 Apple Silicon Mac 上安装 Intel 版 |

默认安装位置：macOS 为 `~/Applications/xrun.app`，CLI 入口为 `~/.local/bin/xrun`；Windows 为 `%LOCALAPPDATA%\Programs\xrun`，CLI 在其 `cli` 子目录；Linux 为 `~/.local/bin/xrun`。Linux 需要 curl、tar、sha256sum，以及 jq 或 Python 3。

脚本在 stdout 输出一条 JSON。成功时包含版本、安装路径、CLI 的 `executable` 路径、`changed` 和自检结果，Windows 另有 `helper_executable`；重复安装相同产物返回 `changed: false`。失败时 `ok` 为 `false`，附 `error.code` 和 `error.message`，退出码为 1。

更新时，macOS 需先关闭 App；脚本会正常停止旧服务再替换文件，替换或自检失败时恢复旧文件。Linux 同样先停止服务。Windows 静默运行安装程序：服务准备失败时不替换程序，返回 32（更新）或 33（卸载），终端配置失败返回 34，诊断写入安装目录的 `xrun-install-error.log`。脚本只负责安装，加入网络和授权仍使用 App 或 CLI。

## 开始使用

一个 xrun 网络包括：所有设备都能访问的**中转**、负责签发成员身份的**管理设备**，以及其他**成员设备**。设备都主动连接中转，不需要开放入站端口。

下面以 Mac `mac-pro` 为管理设备、Linux `linux1` 为成员设备。两台设备先完成安装；每一步都标明在哪台设备上操作。

**1. 准备中转。** 选择一种部署方式，详见 `xrun doc relay`。

使用 Cloudflare 时，在 Mac 上执行，并在浏览器中完成授权：

```bash
xrun relay deploy --provider cloudflare --name xrun-relay
```

使用自建中转时，在 Linux 服务器上执行：

```bash
xrun relay deploy --provider local --addr <服务器IP>:9528
```

成功后输出完整的部署链接：Cloudflare 为 `https://…` 地址，自建中转为 `xrun-relay://…`。记下该链接，下一步在管理设备上使用。

**2. 在 Mac 上创建网络。** 把 `<部署链接>` 换成上一步的完整链接：

```bash
xrun connect '<部署链接>' --name mac-pro
```

成功后显示本机名称和设备 ID，Mac 成为管理设备并启动后台服务。中转部署在哪台机器，不影响谁是管理设备；中转会永久绑定第一个在它上面创建的网络，一个中转只服务一个网络。

**3. 在 Mac 上生成接入码：**

```bash
xrun invite
```

命令输出中转地址和八位接入码，例如 `K7M4-P9Q2`。在 Linux 上执行，将示例地址和接入码替换为实际输出：

```bash
xrun connect relay.example.com K7M4-P9Q2 --name linux1 --allow-manager
```

`--allow-manager` 允许管理设备访问这台新设备，不授予其他权限；省略时交互终端会询问，`--json` 或非交互调用视为拒绝。也可以使用同一接入码的短链接 `xrun://relay.example.com#K7M4-P9Q2`。自动化场景用 `xrun invite --link` 生成完整邀请，再执行 `xrun connect '<邀请>'`。

成功后显示 `linux1` 和设备 ID，并启动 Linux 的后台服务。

**4. 回到 Mac 验证：**

```bash
xrun status
xrun linux1 -- uname -s
```

`status` 中应出现 `linux1`，连接状态为 `relay-connected`；远程命令输出 `Linux` 且退出码为 0，表示连接、授权和执行均已成功。

如果返回 `SOURCE_NOT_ALLOWED`，在 Linux 上执行 `xrun allow-from mac-pro` 后重试；如果返回 `DEVICE_OFFLINE`，先在 Linux 上用 `xrun status` 检查服务，已停止时执行 `xrun daemon start`。其他错误见 `xrun doc errors`。

几条规则：

- 接入码 10 分钟内有效，只能使用一次，最多尝试 5 次；大小写、空格和连字符不影响输入。重新生成会作废上一份，管理设备的服务重启后，未使用的接入码也会失效。
- 加入时管理设备的服务必须在线。加入网络不等于获得访问权限，见 `xrun doc access`。
- **管理设备需要保持在线。** 它定期续签成员有效期，默认 15 分钟。离线超过有效期后，网络中新的远程操作返回 `ROSTER_EXPIRED`，已受理的任务继续运行，见 `xrun doc network`。
- 没有 systemd 用户服务的 Linux 容器，加 `--no-daemon` 只准备身份，再用外部进程管理器运行 `xrun daemon`。
- 也可以使用显式命令：`xrun up --relay <部署链接>` 创建网络并立即输出一份邀请，`xrun join <邀请>` 只接受成员邀请。
- 桌面 App 的“新建网络”可以一次完成第 1、2 步，见 `xrun doc desktop`。

## 中转部署

中转接通设备之间的连接，转发端到端加密的数据。两种中转功能相同：

| | Cloudflare | 自建 Linux |
| --- | --- | --- |
| 运行位置 | 你的 Cloudflare 账号（Worker + Durable Object） | 所有设备都能访问的 Linux 主机 |
| 部署命令 | 可在 macOS、Windows、Linux 上运行 | 在中转主机上运行，需要 systemd |
| 地址 | `https://<名称>.<子域>.workers.dev` 或自定义域名 | `xrun-relay://<地址>/<证书指纹>` |
| 并发会话 | 每个网络 8 个 | 每个来源 16 个、每个目标 32 个 |

### Cloudflare

```bash
xrun relay deploy --provider cloudflare --name xrun-relay
```

命令会打开浏览器，在 Cloudflare 登录并授权 xrun；有多个账号时在终端选择，或用 `--account <账号ID>` 指定。CLI 直接上传内置的 Worker，不需要源码、Node.js 或 Wrangler。账号还没有 workers.dev 子域名时会自动创建；如果 Cloudflare 仍要求初始化，在控制台打开一次 Workers & Pages 后重试。

- 成功后输出完整的 HTTPS 地址。同名重复部署会更新代码，保留地址和数据；同名 Worker 属于其他应用时返回 `ALREADY_EXISTS`，不会覆盖。
- `--region` 为新建的存储对象提供位置提示：`auto`（默认，按连接位置选择）、`wnam`、`enam`、`sam`、`weur`、`eeur`、`apac`、`apac-ne`、`apac-se`、`oc`、`afr`、`me`。只对首次创建的对象生效，实际位置由 Cloudflare 决定。
- 授权默认是临时的，操作完成后立即撤销。部署记录保存在 `~/.xrun/cloudflare/`，以后更新和删除都依赖它，请保留。需要经常维护中转时，可以在创建网络后的管理设备上用 `xrun relay login --provider cloudflare --remember` 把登录保存到系统凭据库（macOS 钥匙串、Windows 凭据管理器、Linux Secret Service），其他设备不能记住登录；`xrun relay logout --provider cloudflare` 撤销并删除登录，已部署的中转保留。
- 自定义域名：在 Cloudflare 控制台的 Worker → Settings → Domains & Routes 中添加 Custom Domain（域名须在你自己的 Cloudflare zone 中），创建网络时直接使用 `https://relay.example.com`。已有网络改用新地址时，在管理设备上停止服务，执行 `xrun up --relay 'https://relay.example.com'`，再启动服务。
- 每个方向最多 4 MiB 未确认的数据在途，单个文件大小没有固定上限。

### 自建 Linux 中转

在中转主机上执行：

```bash
xrun relay deploy --provider local --addr 203.0.113.10:9528
```

命令安装 systemd 用户服务并启用 linger，中转随开机启动，异常退出后自动重启。默认监听 TCP 9528，仅支持 IPv4，防火墙和安全组的端口需要自行放行。

| 选项 | 作用 |
| --- | --- |
| `--addr <主机>:<端口>` | 设备连接的地址，可重复或用逗号分隔，指定后持久保留；域名需要有 A 记录 |
| `--port <端口>` | 实际监听端口，默认 9528；可以与发布地址中的端口不同（端口映射） |
| `--no-detect` | 省略 `--addr` 时只使用网卡地址，不查询公网出口 IP |

省略 `--addr` 时自动探测网卡 IPv4 和公网出口地址。出口地址不保证外部可达，长期使用建议固定 IP 或域名。部署链接包含中转证书指纹，可以重复使用，重启后不变；它不是访问凭据。中转主机也可以另外作为普通成员加入网络，两者互不影响。

### 查看、检查与移除

```bash
xrun relay link --provider cloudflare --name xrun-relay    # 从本机记录读取地址，不联网
xrun relay check --provider cloudflare --name xrun-relay   # 检查连通和协议，最多 30 秒
xrun relay remove --provider cloudflare --name xrun-relay --purge
xrun relay link --provider local
xrun relay remove --provider local
```

`relay check` 不重新授权，不上传代码，也不创建网络。删除 Cloudflare 中转必须加 `--purge`，会永久删除 Worker、全部中转数据和流量统计；只接受本机有部署记录、且远端归属标记匹配的实例。删除失败可以重复执行；删除开始后如需重新部署，请换一个名称。`remove --provider local` 卸载服务，保留配置和统计。`xrun relay run --provider local` 在前台运行已配置的本地中转，用于调试或交给外部进程管理。

### 部署进度与自动化

普通模式在 stderr 显示各阶段和耗时，stdout 只输出最终地址和下一步命令。脚本和 AI 使用 `--json`：

```bash
xrun relay deploy --provider cloudflare --name xrun-relay --no-browser --json
xrun relay check --provider cloudflare --name xrun-relay --json
```

- stdout 只有一条最终 JSON，stderr 是逐行 JSON 事件。进度事件的 `event` 为 `deployment_progress`，包含 `operation_id`、`sequence`、`timestamp_ms`、`stage`、`status`、`elapsed_ms` 和 `stage_elapsed_ms`；等待授权时附 `authorization_url`，需要选择账号时附 `accounts`，中转保存后附 `deployment` 和 `next_command`。事件中不含授权码或令牌。请按阶段和错误码判断，不要解析提示文字。
- `--no-browser` 只是不自动打开浏览器，命令仍会等待授权，最长 5 分钟。授权页必须在运行命令的同一台机器上打开，回调使用 `127.0.0.1` 的 8976、8977 或 8978 端口。
- 无人值守部署可以用 `CLOUDFLARE_API_TOKEN` 提供 API Token，并用 `--account` 或 `CLOUDFLARE_ACCOUNT_ID` 指定账号；令牌不会写入部署记录。JSON 模式下有多个账号但未指定时，返回 `INTERACTIVE_REQUIRED`。
- 结果包含 `deployed`、`ready`、`authorization_cleanup` 和 `next_command`。Worker 已保存、但连接检查或授权清理失败时，stdout 仍返回已创建的中转和 `error`，退出码非零。此时不要换名重新部署：先运行 `relay check`，恢复后执行 `xrun up --relay <地址> --name <设备名> --json`。
- Ctrl+C 取消操作并以 130 退出，已有的部署记录保留，用同一 `--name` 即可继续。强制退出或授权撤销失败时，可以在 Cloudflare 账号中手动撤销 xrun 的授权。
- `--dry-run --json` 只检查内置的 Worker，不登录，也不修改 Cloudflare。
- 各阶段耗时记录在 `~/.xrun/deployment.log`（约 1 MiB 封顶），不含授权地址、令牌或错误正文。

## 访问权限

xrun 把三件事分开：管理设备决定**谁是成员**（邀请、撤销），目标设备决定**谁能访问自己**，中转只负责转发。成员身份本身不带任何执行、文件、截图或转发权限。

在目标设备上授权，例如允许 mac-pro 访问本机：

```bash
xrun allow-from mac-pro    # 允许
xrun deny-from mac-pro     # 拒绝，并关闭它的现有会话
xrun allow-from --all      # 信任所有成员，包括以后加入的
xrun deny-from --all       # 关闭全体信任，保留单独的允许和拒绝
xrun daemon pause          # 暂停全部远程访问
xrun daemon resume         # 按原授权恢复
```

- **允许访问，就是交出目标设备当前用户的权限**：执行命令、读写文件、截图和端口转发。只授权可信的设备。
- 判断顺序：未撤销的成员 → 本机未暂停 → 未被单独拒绝 → 在允许列表中，或开启了全体信任。授权保存在本机，重启后保留。
- 授权是单向的，双向访问需要两台设备分别 `allow-from` 对方。`xrun invite --allow`（或 `up --allow`）只是请求新设备允许管理设备访问，由新设备确认；邀请从不让新成员获得访问管理设备的权限。
- 管理设备默认拒绝远程操作，可以逐台 `allow-from`，但不接受 `allow-from --all`（返回 `MANAGER_PROTECTED`）。被允许的设备能接触管理设备上的网络管理密钥，请格外谨慎。
- 设备名只是别名，授权和记录都使用不可变的设备 ID；命令也接受 32 位十六进制的设备 ID。
- `deny-from` 和暂停会关闭现有会话、流式执行和端口转发；已受理的可靠任务继续运行。

撤销成员只能在管理设备上执行：

```bash
xrun revoke win1
```

管理设备签署新的成员清单，同步给中转和在线设备，并列出尚未确认收到的设备。收到更新的设备立即拒绝被撤销的设备；已受理的任务不会被取消或回滚。离线设备上线后才能收到更新，需要立即阻断时，在目标设备上执行 `deny-from` 或暂停访问。被撤销的密钥不能重新加入。

### 安全模型

- 设备之间在中转隧道内建立双向 TLS，用网络自己的根证书验证身份。命令、参数、环境变量、输入输出、文件和截图只有通信两端可见。
- 中转能看到网络 ID、连接时出示的成员证书、来源 IP、在线状态、连接时间和流量大小；它保存网络归属、管理设备签名的成员清单（设备 ID、名称、密钥指纹和撤销状态）及其有效期，以及流量统计。中转不能伪造成员、授予权限或解密业务数据，但可以中断连接或延迟转发更新。
- 网络根私钥只在管理设备上。丢失后无法再签发或撤销成员，请用 `xrun network backup` 加密备份。
- 本机数据目录 `~/.xrun` 包含私钥、数据库和日志，应按敏感数据保管。远端输出原样传回，终端控制字符和提示注入需要调用方自行防范。

## 网络管理

`xrun network` 管理成员有效期、局域网直连、传输压缩和管理身份。桌面 App 在“设置”中提供相同的选项。

### 成员有效期

```bash
xrun network lease       # 查看
xrun network lease 60    # 在管理设备上设为 60 分钟（1–1440）
```

管理设备在线时，大约每过三分之一有效期就续签一次成员清单，默认有效期 15 分钟。设备和中转只接受有效期内的清单：管理设备离线超过有效期后，新的远程操作（包括任务查询）返回 `ROSTER_EXPIRED`；已受理的任务继续运行，本机活动照常可看。管理设备恢复在线后自动续签。

有效期越长，管理设备可以离线越久；但中转如果扣住撤销更新，旧清单也能继续使用更久。各设备的时钟应保持同步。此外，成员证书最长有效 365 天，剩余不足 30 天时自动续期，续期同样需要管理设备在线。

### 局域网直连

```bash
xrun network direct                 # 列出本机内网 IPv4 接口和当前设置
xrun network direct 192.168.1.10    # 在该内网地址上启用直连
xrun network direct --off
```

两台设备都选择了同一内网的接口时，连接会优先尝试直连。直连只在所选地址的随机端口上监听，不扫描网络，也不做 UPnP；地址只通过已认证的加密连接交换，身份验证和授权与走中转时相同。直连失败时，在发送请求前自动改走中转。直连流量不计入中转统计，设备信息会显示上次连接是“直连”还是“中转”。

### 传输压缩

```bash
xrun network compression          # 查看
xrun network compression auto     # 默认：至少节省 1/8 时才压缩
xrun network compression none     # 不压缩
xrun network compression zstd     # 总是尝试 zstd，但不会让数据变大
```

设置只影响本机发出的文件和日志。PNG、JPEG、WebP、ZIP、GZIP、Zstd 和 MP4 等已压缩格式会直接跳过，压缩也不改变落盘的内容。

### 备份与交接管理身份

以下命令在终端中输入隐藏密码，密码至少 12 个字符。自动化可以用 `--password-stdin` 从 stdin 读取一行。备份包含管理身份、网络根私钥和成员数据库快照，不包含 Cloudflare 登录凭据。

下文用 CLI 服务命令演示启停；由 App 管理的服务，可在“设置 → 网络与服务”中完成启停。

#### 备份与恢复

在当前管理设备上生成加密备份，并将文件和密码分别妥善保存：

```bash
xrun network backup ./xrun-manager.backup
```

需要恢复时，先把备份复制到恢复用的设备，再在该设备上执行：

```bash
xrun daemon stop
xrun network restore ./xrun-manager.backup --json
xrun daemon start
xrun status --json
```

新安装、尚未注册服务的设备可以省略 `stop`，恢复后用 `xrun daemon install` 代替 `start`。成功后，`restore` 返回的 `manager_id` 应与 `status --json` 中的 `local.device_id` 一致；服务连接后，`local.daemon_connected` 为 `true`。

密码错误或文件损坏时不会覆盖现有身份。恢复会核对中转上的最新成员清单，保留之后发生的撤销，并清空允许访问列表，需要重新 `allow-from`。同一份管理身份恢复到另一台机器后，原机器发现新的管理代次时会删除根私钥并停止服务。

#### 交接给已有成员

下面把管理身份交给已加入网络的 Linux 设备 `linux1`。先确认它允许当前管理设备访问，然后在**旧管理设备**上生成并传送交接文件：

```bash
xrun network migrate linux1 --out ./xrun-handover.backup
xrun linux1 push ./xrun-handover.backup /tmp/xrun-handover.backup --no-overwrite
```

生成文件后，旧管理设备照常工作。文件传送成功后，在 **linux1 本机**停止服务、导入并重新启动：

```bash
xrun daemon stop
xrun network restore /tmp/xrun-handover.backup --json
xrun daemon start
xrun status --json
```

确认 `restore` 返回的 `manager_id` 与 `status --json` 中的 `local.device_id` 一致，且 `local.daemon_connected` 为 `true`。旧管理设备收到更新后会自动撤销 Cloudflare 登录、删除根私钥，成为普通成员。其他设备的访问授权不会自动转给 linux1，需要在各目标设备上单独 `allow-from linux1`。

密码错误或校验失败时保留原有身份，可修正后重新导入。网络始终只有一台管理设备，不支持多管理设备或根密钥轮换。

## 执行程序

```bash
xrun linux1 -C /home/user/demo -- cargo test
xrun win1 -C 'D:\demo' -- cargo build
xrun linux1 --env RUST_LOG=info --env MODE=test -C /home/user/demo -- ./demo
```

`--` 之后的内容原样交给远端程序。xrun 直接启动程序，不经过 Shell，所以管道、重定向和变量展开都由本机 Shell 处理。目标用自己后台服务的 PATH 查找程序（见 `xrun doc config`），找不到时请写完整路径。

需要 Shell 时，用 `--script` 从 stdin 传入脚本，并明确指定 `sh`、`bash`、`zsh`、`powershell`、`pwsh` 或 `cmd`：

```bash
xrun linux1 -C /repo --script bash < ./build.sh
xrun win1 -C 'D:\demo' --script powershell < ./build.ps1
printf 'hello\n' | xrun linux1 --stdin -- cat
```

PowerShell 脚本使用 UTF-8；cmd 脚本只接受 ASCII，脚本参数不能包含双引号或换行。Windows 批处理文件需要通过 `--script cmd` 或 `cmd.exe /C` 运行，否则返回 `SHELL_REQUIRED`。

| 选项 | 作用 |
| --- | --- |
| `-C <目录>` | 远端绝对工作目录，省略时使用目标的默认目录（默认为主目录） |
| `--env KEY=VALUE` | 设置远端环境变量，可重复 |
| `--stdin` | 提交前读完本地 stdin 并一起发送，最多 1 MiB |
| `--script <shell>` | 从 stdin 读取脚本，最多 1 MiB |
| `--timeout <秒>` | 超时时间，前台执行默认 1800，0 表示不限 |
| `--request-id <ID>` | 恢复同一次提交，见 `xrun doc errors` |
| `-i` | 流式执行，见 `xrun doc streaming` |

前台执行原样输出远端的 stdout 和 stderr，并以远端退出码退出。它是**可靠任务**：一旦受理，目标会执行到结束并保存结果，CLI 断线不影响任务。CLI 会在 30 秒内尝试重连并补读日志，仍无法确认结果时返回 75。

目标默认最多同时运行 4 个命令（含流式执行），已满时返回 `DEVICE_BUSY`，不排队。本机后台服务运行时，CLI 会复用到同一目标的加密连接，省去重复握手。

## 任务与日志

`start` 在任务受理后立即返回任务引用，例如 `linux1/ABC123`；后台任务默认不限时：

```bash
xrun linux1 start -C /home/user/demo -- ./server
xrun linux1 jobs --running
xrun linux1 jobs ABC123 --json
xrun linux1 logs ABC123 --tail 40
xrun linux1 logs ABC123 --follow
xrun linux1 wait ABC123 --timeout 60
xrun linux1 kill ABC123
```

把 ABC123 换成实际返回的六位 ID，也可以直接使用完整引用。自动化可以用 `start --json` 读取任务 ID。

- `jobs` 列出本机在目标上提交的所有操作，包括命令、流式执行、文件传输、截图和端口转发。默认每页 50 条，用 `--limit`、`--offset` 翻页，`--running`、`--request-id` 过滤。指定 ID 时查看单个任务，此时不能使用过滤选项。
- `logs` 只适用于普通命令。默认读取当前快照；`--follow` 补读后持续跟随；`--after N` 从序号 N 之后读；`--tail N` 只取最后 N 行，由目标端截取。`--after` 和 `--tail` 不能同时使用。
- `wait` 等到任务结束，再显示最后 40 行（用 `--tail` 修改）。`--timeout` 只限制等待时间，不取消任务；无法确认结果时返回 75。
- `kill` 请求取消并等待确认。Unix 先发 SIGTERM，5 秒后发 SIGKILL；Windows 结束任务的整个进程树。
- 每台设备只能查询和控制自己提交的任务。
- `xrun recent` 列出本机最近 24 小时的提交记录，用于找回 request_id；它不代表远端任务的当前状态。

| 状态 | 含义 |
| --- | --- |
| `accepted` / `running` | 已受理 / 正在运行 |
| `succeeded` | 成功 |
| `failed` | 失败，包括非零退出；结果中保留退出码和错误 |
| `canceled` / `timed_out` | 已取消 / 已超时，受管理的进程都已停止 |
| `interrupted` | 文件传输中断，等待用 `resume` 继续 |
| `lost` | 目标服务崩溃等原因导致结果无法确认，可能已经执行过 |

任务不会自动重跑。目标服务正常停止时会取消运行中的任务；崩溃重启后，未完成的任务标记为 `lost`。每个任务最多保存 64 MiB 日志（超出部分标记 `TRUNCATED`），全部日志最多 1 GiB；已结束任务的日志保留 7 天，任务记录本身长期保留。

## 自动化与 AI

先用 `xrun doc --list` 找章节、用 `xrun help <命令>` 确认参数。组网完成后，以下流程可用于 AI agent 和脚本；示例目标是已授权的 Linux 设备 `linux1`。

### 提交、查询与等待

**1. 检查本机与目标。** 在发起操作的设备上执行：

```bash
xrun status --json
xrun linux1 info --json
```

`status` 用于发现设备和检查本机状态；它正常退出时仍可能带有非空的 `server_error`，此时先处理发现错误。确认 `info` 返回的 `online` 为 `true` 后再提交。首次组网见 `xrun doc quickstart`，Cloudflare 自动部署见 `xrun doc relay`。

**2. 提交任务。** 为每个新任务生成并保存一个唯一的 `request_id`，重试时沿用。下面的 UUID 仅作示例：

```bash
xrun linux1 start --json --request-id 9cb402ae-47fa-4a58-94ac-e84ffac87ca9 -- echo xrun-ok
```

退出码为 0 表示任务已受理。stdout 是 Job 对象，例如（仅保留关键字段）：

```json
{
  "job_id": "ABC123",
  "request_id": "9cb402ae-47fa-4a58-94ac-e84ffac87ca9",
  "state": "accepted"
}
```

**3. 用返回的 `job_id` 查询或等待。** 将 ABC123 换成实际值：

```bash
xrun linux1 jobs ABC123 --json
xrun linux1 wait ABC123 --timeout 60 --json
```

`jobs <ID>` 返回 Job 对象，`jobs --request-id <ID>` 返回 Job 数组，`wait` 返回包含 `job`、`logs` 和 `logs_error` 的对象。任务成功后，`wait` 的 stdout 例如（仅保留关键字段）：

```json
{
  "job": {
    "job_id": "ABC123",
    "state": "succeeded",
    "result": { "exit_code": 0, "signal": null }
  },
  "logs": [
    { "seq": 1, "stream": "stdout", "data_base64": "eHJ1bi1vawo=" }
  ],
  "logs_error": null
}
```

日志中的 Base64 解码为 `xrun-ok\n`。`start` 成功后还需检查最终的 `job.state` 和 `job.result`；`wait` 的退出码反映任务结果。`logs_error` 表示读取日志遇到问题，不改变已确认的任务结果。

### 失败后的下一步

例如，`wait --timeout 60 --json` 等待超时，退出码为 75、stdout 为空，stderr 返回：

```json
{
  "type": "error",
  "code": "WAIT_TIMEOUT",
  "message": "waiting for task timed out; query jobs/logs for its result"
}
```

等待超时不会取消或重新启动任务。根据操作选择下一步：

| 情况 | 下一步 |
| --- | --- |
| `start` 返回 75，尚未拿到 `job_id` | 用原 `request_id` 执行 `jobs --request-id <ID> --json`；找到后查询或等待该 Job |
| `wait` 返回 75，尚无已确认的终态 Job | 再次查询或等待同一个 Job |
| 已确认 `failed`、`timed_out` 或 `canceled` | 读取结果和日志，决定是否用新的 `request_id` 发起新任务 |
| `lost` 或 `DB_RESET` | 结果无法确认，核实实际操作结果后再决定是否重新执行 |
| 文件传输中断 | 使用 `xrun <设备> resume <request_id>`，见 `xrun doc files` |

先检查 stdout 中是否已有终态 Job：远端程序本身也可能返回 75，此时按已确认的任务结果处理。查不到 Job 不代表操作未执行。需要恢复同一次命令提交时，保持原目标、参数、输入和 `request_id`，见 `xrun doc errors`；不要因为查询失败就换新 ID 重跑。

stdout 和 stderr 分开读取，按退出码、结构化字段和错误码判断。stderr 可能包含多条诊断或进度事件，部分参数错误仍为文本，不要把整段 stderr 当成一个 JSON。安装脚本使用 `ok` 字段，上述 CLI 命令直接返回各自的数据结构。

## 文件与截图

push 和 pull 一次传一个文件，参数按“源、目标”的顺序：

```bash
xrun linux1 pull /home/user/demo/config.json ./config.json --json
xrun linux1 push ./config.json /home/user/demo/config.json --expect '<pull 返回的 sha256>'
xrun linux1 push ./result.txt /home/user/demo/out/result.txt --mkdir
xrun linux1 pull /repo/config.json -               # 输出到 stdout
printf 'hello\n' | xrun linux1 push - /repo/note.txt
```

- 原样传输字节，不转换编码、BOM 或换行。按 64 KiB 分块传输，文件大小没有固定上限。完整接收并通过 SHA-256 校验后才原子替换目标，传输中断不会损坏已有文件。
- 远端相对路径相对于 `-C` 或目标的默认目录，本地相对路径相对于当前目录。`--mkdir` 创建远端父目录。pull 省略本地路径时保存到临时文件，stdout 输出它的绝对路径。
- `--expect <sha256>` 只在远端文件仍是该内容时覆盖，否则返回 `STALE`；`--no-overwrite` 要求目标不存在，否则返回 `ALREADY_EXISTS`。两者不能同时使用。
- 上传内容与远端文件完全相同时跳过数据传输，但仍记录一次操作。
- pull 保存到本地时拒绝写入符号链接；push 写入远端时跟随符号链接。
- 传输中断后，CLI 会提示 `xrun <设备> resume <request_id>`。24 小时内可以从首次传输时保存的副本继续同一次传输；网络、数据库、身份或内容摘要变化时拒绝继续。源文件已经改动时，请重新 push。push 的响应丢失时不会自动重发，请先 pull 确认目标内容。
- 传输目录可以把 tar 与流式执行组合使用，见 `xrun doc streaming`；查看目录用远端命令，例如 `xrun linux1 -- ls -la /repo`。

截图拍摄目标的主显示器：

```bash
xrun win1 screenshot ./screen.png
xrun win1 screenshot ./screen.jpg --format jpeg --max-width 1600 --quality 80
```

默认保存原始尺寸的 PNG。`--format` 可选 `png`、`jpeg`、`webp`；`--quality`（1–100）只用于 JPEG 和 WebP；`--max-width` 只缩小、不放大。省略路径时保存到临时文件，stdout 输出路径，可以直接交给 AI 看图。

| 平台 | 要求 |
| --- | --- |
| macOS | 服务运行在用户登录会话中，并已获得“屏幕录制”权限，否则返回 `PERMISSION_DENIED` |
| Windows | 服务运行在交互桌面中；锁屏时返回 `SCREEN_LOCKED` |
| Linux | 仅支持 X11；Wayland 返回 `SCREENSHOT_UNAVAILABLE`，没有图形会话返回 `NO_DISPLAY` |

网页截图不依赖桌面：在远端运行项目自己的无头浏览器脚本，再用 pull 取回图片。目标设备会为收发的文件和截图保留一份附件副本，见 `xrun doc desktop`。

## 流式执行

`-i` 实时传递 stdin、stdout 和 stderr，适合大量输入或与本地管道组合，输入没有 1 MiB 的限制：

```bash
xrun linux1 -i -- cat < ./large.bin > ./copy.bin
xrun linux1 -i -C /repo -- tar -cf - artifacts > ./artifacts.tar
```

- 不提供终端（PTY），不能运行交互式 Shell 或全屏程序。
- 本地 stdin 结束后，远端 stdin 随之关闭，输出继续传回。默认超时 1800 秒。
- Ctrl+C（返回 130）或断线都会结束远端进程，不会自动重跑。需要断线后继续运行时，请使用普通执行或 `start`。
- 会创建一条任务记录，可以使用 `jobs`、`wait` 和 `kill`；但输出不保存，`logs` 返回 `LOG_UNAVAILABLE`。
- 不能与 `start`、`--json`、`--stdin`、`--script` 或 `--request-id` 同时使用。

## 端口转发

`forward` 把远端的回环端口映射到本机的回环端口，例如在本机浏览器中访问远端的开发服务：

```bash
xrun linux1 forward 8080:3000       # 本机 127.0.0.1:8080 → linux1 的 127.0.0.1:3000
xrun linux1 forward 3000            # 两端使用相同端口
xrun linux1 forward 0:3000 --json   # 由系统分配本地端口，JSON 返回 local_address
```

- 本机只监听 `127.0.0.1`，目标只连接自己的 `127.0.0.1`（失败时尝试 `::1`），不能访问对方网络中的其他主机。
- 命令保持前台运行，Ctrl+C 结束监听和所有连接。中断的连接不会恢复，新的连接会重新接通。
- 每条 TCP 连接占用一个中转会话，每个转发最多 32 条连接；Cloudflare 中转每个网络最多 8 个会话。
- 本机的其他进程也能访问这个端口。目标暂停访问、拒绝或撤销来源时，转发连接随即关闭。

## 桌面 App

macOS 和 Windows 的桌面 App 负责组网、授权、后台服务和本机活动，也可以向其他设备执行命令、传输文件和截图，安装见 `xrun doc install`。App 只运行一个实例，关闭窗口后留在菜单栏或系统托盘中。

**连接设备。** 未加入网络时，App 显示“连接你的设备”：

- **接入码**：输入中转地址和接入码，或粘贴短链接。
- **使用链接**：粘贴部署链接或成员邀请，自动识别是创建还是加入。
- **新建网络**：点击“授权并创建”，在浏览器授权 Cloudflare 后，App 自动部署中转、创建网络并启动服务。“中转选项”中可以修改中转名称和区域；有多个账号时先选择账号。中转已部署但网络创建失败时，重试会继续使用这个中转。

加入时可以勾选“允许管理设备访问本机”；不勾选则明确拒绝，即使邀请中附带了访问请求。本机名称默认使用主机名。已有的 CLI 身份会直接沿用。

**界面。** 侧栏包含“设备”“活动”“设置”三个页面和本机卡片（名称、角色、状态和“暂停访问”按钮），点击卡片进入网络设置。需要系统授权、后台服务已停止或远程访问已暂停时，页面顶部会提示并提供对应操作。

- **设备**：列出网络中的其他设备，可以按在线状态筛选和搜索。每台设备显示系统、架构和短 ID，“允许访问本机”开关控制它能否访问本机。在线设备可以直接“执行命令”“传输文件”和“截图”，前提是对方允许本机访问。菜单中可以查看设备 ID、版本、最近在线时间和上次连接方式（直连／中转），管理设备还可以撤销成员；已撤销的成员单独折叠显示。管理设备通过“邀请设备”生成接入码，并可复制接入命令或短链接。页面底部显示今日流量，点击“详情”查看趋势和各设备统计。
- **执行命令**：填写命令、解释器、远程工作目录和超时，查看输出、状态和退出码，可以取消任务。关闭窗口不影响已受理的任务；“最近提交”保留 24 小时内的最近 20 条，结果不确定时可以查询结果，不会重复执行。
- **传输文件**：上传或下载单个文件，可以创建目录、允许覆盖；中断的传输可以“继续”或“放弃”。
- **截图**：可选原图 PNG、省流，或自定义格式、最大宽度和画质。
- **活动**：其他设备在本机执行的所有操作，按日期分组，可以按进行中、失败筛选，并搜索当前页。详情显示工作目录、耗时、退出结果和输出；文件和截图附件可以预览（8 MiB 以内的 PNG/JPEG、1 MiB 以内的文本），或另存为。活动读取本机数据库，服务停止或断网时也能查看。
- **设置**：
  - 通用：语言（跟随系统、中文、English）、局域网直连、省流量、登录时显示 xrun 图标、隐藏菜单栏／托盘图标。
  - 活动与附件：附件保留时间，默认 30 天，0 表示长期保留。
  - 执行环境：默认工作目录、同时运行的任务数和 PATH，保存后对新任务生效。
  - 网络与服务：网络信息、允许全体成员访问本机、启动或停止服务、成员有效期（管理设备）、本机部署的中转（授权账号、记住登录、复制部署链接）、管理身份（备份、交接、恢复）和移除后台服务。
  - 关于：版本、系统和数据目录。

**菜单栏与托盘。** 菜单显示本机状态，提供“打开 xrun”“活动”“设置…”，以及随状态变化的主操作：加入网络…、启动服务、暂停访问或恢复访问。Windows 上双击图标也能打开窗口。隐藏图标后，重新打开 App 即可恢复。

**后台服务。** 执行和传输由独立的后台服务完成：

- “退出 xrun”只退出界面，服务继续运行。
- “停止服务”（设置 → 网络与服务）正常结束服务，并取消本机正在运行的任务；下次登录时服务会再次启动。
- “移除后台服务”取消登录启动，保留本机数据。
- macOS 通过系统登录项注册服务，首次可能需要在“系统设置 → 通用 → 登录项”中允许 xrun；截图需要“屏幕录制”权限。更换 App 后，下次启动服务时会重新登记当前程序。
- Windows 的服务运行在当前用户的登录会话中，不显示控制台窗口。
- 从 CLI 安装的服务切换到 App：先执行 `xrun daemon stop`，再在 App 中启动服务，App 会替换服务注册。

## 配置与服务

`connect`、`up` 和 `join` 默认安装后台服务，以当前用户身份运行：Linux 为 systemd 用户服务（并启用 linger），macOS 为 LaunchAgent，Windows 为登录时运行的计划任务。桌面 App 使用自己的服务注册，见 `xrun doc desktop`。

```bash
xrun daemon install      # 注册并启动服务（需要已有设备身份）
xrun daemon start
xrun daemon stop         # 正常停止，取消本机运行中的任务
xrun daemon uninstall    # 移除注册，保留数据
xrun daemon              # 在前台运行
xrun daemon reset        # 服务停止时重建任务库，保留设备身份
```

没有 systemd 用户会话的 Linux 容器，用 `--no-daemon` 加入，再由外部进程管理器运行 `xrun daemon`。`status` 中的 `daemon_installed` 只表示服务注册存在，请结合 `daemon_running` 和 `daemon_connected` 判断。

配置、身份和数据库位于 `~/.xrun`（Windows 为 `%USERPROFILE%\.xrun`），只有当前用户可以访问。执行环境和授权写在 `daemon.toml` 中：

```toml
allow_from = ["b63c4960932e4b46afdb8028e9490cc8"]
default_cwd = "/home/user/repo"
max_concurrent_jobs = 4
attachment_retention_days = 30

[env]
PATH = "/home/user/.cargo/bin:/home/user/.local/bin:/usr/local/bin:/usr/bin:/bin"
```

- 修改后对新任务生效，无需重启服务；程序保存设置时会保留其他字段。路径中的 `~` 不会展开，请写目标用户的实际主目录；Windows 的 PATH 用分号分隔。
- 后台服务通常不加载交互 Shell 的配置，工具链目录（如 `~/.cargo/bin`）需要写进 `[env]` 的 PATH。
- 任务继承服务的环境，但不继承这些构建变量：`CARGO_TARGET_DIR`、`CARGO_BUILD_TARGET`、`RUSTUP_TOOLCHAIN`、`RUST_RECURSION_COUNT`、`RUSTC`、`RUSTDOC`、`RUSTC_WRAPPER`、`RUSTC_WORKSPACE_WRAPPER`、`RUSTFLAGS`、`CARGO_ENCODED_RUSTFLAGS`。需要时写入 `[env]`，或用 `--env` 传入。
- `max_concurrent_jobs` 是同时运行的命令数，范围 1–64。
- 目标设备为收发的文件和截图保留独立的附件副本。`attachment_retention_days` 默认 30，0 表示长期保留，最大 3650；每 10 分钟清理一次到期的副本，活动摘要保留。

服务日志与任务日志不同：macOS 上 CLI 安装的服务写入 `~/.xrun/daemon-service.log`，Linux 用 `journalctl --user -u xrun-daemon.service` 查看，其他情况请查看外部进程管理器的日志。

## 状态与排错

```bash
xrun status --json          # 本机服务、授权和在线设备
xrun linux1 info --json     # 连接目标，读取系统、版本、主机名和默认目录
xrun recent --json          # 本机最近的提交
xrun traffic --period today
```

`status` 的在线状态来自中转，只说明设备连着中转，不保证能执行命令；要确认目标是否可用，请用 `info`。突然断网的设备通常在 45–60 秒后显示为离线。

`--json` 的数据写到 stdout，诊断信息写到 stderr。前台执行不支持 `--json`，原样传递远端的输出。

### 退出码

| 退出码 | 含义 |
| --- | --- |
| 0–255 | 远端程序的退出码（Windows 上大于 255 时为 1，完整值在任务结果中） |
| 128+N | 远端进程被信号 N 结束 |
| 124 | 超时，进程已停止 |
| 130 | Ctrl+C：已确认取消，或请求尚未受理 |
| 75 | **结果未确认**：请求可能已经送达，先查询再决定是否重试 |
| 125 | 连接、身份、授权或执行层错误 |
| 2 | 本地参数错误 |
| 1 | 文件、截图等操作已确认失败 |

远端程序也可能返回这些数值，请结合 stderr 中的 `[xrun]` 诊断、错误码和任务状态判断。

### 结果未确认（75）

先查询，不要直接重新执行：

```bash
xrun recent                                   # 找到 request_id
xrun win1 jobs --request-id '<request_id>'
xrun win1 wait <任务ID>
```

要恢复同一次提交，用原参数、原输入和原 `--request-id` 重新执行：目标按来源和请求 ID 去重，已受理的请求不会再次运行。普通调用总会生成新的 request_id。目标的任务库已重建时返回 `DB_RESET`。

Ctrl+C 会尝试取消可靠任务，无法确认时返回 75；SIGTERM 和 SIGHUP 不会取消已提交的任务。

### 流量统计

`xrun traffic` 查询中转记录的流量，不需要本机服务运行。管理设备可以看到全网总量和各设备明细，其他成员只能看到自己。`--period` 可选 `today`、`month`（默认）和 `all`，按 UTC 日期统计；设备明细用 `--limit`（1–256）和 `--offset` 翻页，JSON 中的 `next_offset` 指向下一页。

统计的是设备之间的加密数据（含 TLS 握手和协议开销），不含认证、确认和心跳。入站指设备发给中转的数据，出站指中转发给设备的数据；计算转发量时只看出站，不要与入站相加。统计从中转开始运行时累积，更换中转不会合并，直连流量也不计入。录入出错时报告 `complete: false`。这些数字不是计费账单，xrun 也没有流量额度或限速。

### 常见错误码

| 错误码 | 处理 |
| --- | --- |
| `SOURCE_NOT_ALLOWED` | 在目标设备上 `allow-from <来源>` |
| `ACCESS_PAUSED` | 目标暂停了访问，在目标上 `xrun daemon resume` |
| `ROSTER_EXPIRED` | 管理设备离线超过成员有效期，启动管理设备的服务 |
| `DEVICE_OFFLINE` | 检查目标服务、中转和网络；xrun 不排队执行 |
| `DEVICE_BUSY` | 目标同时运行的命令已满，稍后用原 request_id 重试 |
| `DEVICE_REVOKED` | 目标或本机已被撤销 |
| `INVALID_CODE`、`CODE_EXPIRED`、`CODE_LOCKED` | 核对中转地址和接入码；过期或次数用完时重新生成 |
| `CODE_IN_USE`、`PAIRING_BUSY` | 接入码正被其他设备使用，为这台设备生成新码，或稍后重试 |
| `RELAY_CLAIMED` | 中转已属于另一个网络，向该网络的管理设备索取邀请 |
| `RELAY_UNCLAIMED` | 中转尚未绑定网络，在管理设备上创建网络，或启动管理设备的服务 |
| `NOT_MANAGER`、`MANAGER_PROTECTED` | 只能在管理设备上执行；管理设备不接受全体信任 |
| `VERSION_MISMATCH` | 本机 CLI、App 与服务版本不一致，或设备之间协议不兼容；统一升级并重启服务 |
| `PROGRAM_NOT_FOUND`、`SHELL_REQUIRED` | 检查目标的 PATH 或写完整路径；批处理使用 `--script cmd` |
| `INVALID_CWD` | `-C` 需要是目标上存在的绝对目录 |
| `STALE`、`ALREADY_EXISTS` | 远端文件已变化或已存在，先 pull 确认 |
| `PERMISSION_DENIED`、`SCREEN_LOCKED`、`NO_DISPLAY` | 缺少截图权限、屏幕已锁定或没有图形会话 |
| `INTERACTIVE_REQUIRED` | 需要交互终端，或补充 `--account`、`--password-stdin` 等参数 |
| `DB_RESET` | 目标的任务库已重建，原任务无法查询或重放 |
| `DB_SCHEMA_MISMATCH` | 数据库格式与程序版本不符，程序不会改动原数据，见 `xrun doc upgrade` |

## 升级与移除

当前仍为预发布版本（`-rc`），不同 RC 之间不保证协议或数据兼容。升级时，将所有设备上的 CLI、App、后台服务，以及中转统一到同一 RC 版本后再继续操作。

同一台设备上的 CLI、App 和后台服务必须是同一版本，替换程序后需要重启服务。当前协议、签名格式和数据库 schema 的编号均为 1，共用编号不代表不同 RC 可以混用。正式版的兼容范围和升级方式会在正式发布时明确。

```bash
xrun daemon stop       # 会取消运行中的任务，请先等它们结束
# 替换程序，或运行新版本的安装脚本
xrun --version
xrun daemon start
xrun status
```

安装脚本会自动停止服务并替换程序。桌面 App 更新后打开时，会停止版本不符的服务，换用新版本重新启动；原本已停止的服务保持停止。

移除服务和数据：

```bash
xrun down            # 移除本机服务，保留数据
xrun down --purge    # 交互确认后删除本机身份和全部数据
```

- Linux 上 `down` 同时移除本机的中转服务。由 App 管理的服务，请在 App 中“移除后台服务”。
- 管理设备执行 `--purge` 会删除网络根私钥，之后无法再签发或撤销成员；除非能从备份恢复，否则需要重新创建网络，所有成员重新加入和授权。
- 成员设备执行 `--purge` 后只能作为新设备重新加入，旧身份应由管理设备撤销。
- 只想清空任务记录时，停止服务后执行 `xrun daemon reset`：设备身份保留，旧的任务引用不能用于新库。
- 遇到 `DB_SCHEMA_MISMATCH` 时，不要删除身份文件或 `roster.db`，后者保存着最新的撤销状态。

卸载程序：macOS 先移除后台服务，再删除 `xrun.app` 和 `~/.local/bin/xrun`；Windows 在系统设置的“应用”中卸载；Linux 删除 `xrun` 程序即可。
