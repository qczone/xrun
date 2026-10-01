# xrun 设计文档

核心功能已实现。Linux/macOS 执行与传输、Linux X11 截图已有运行证据；Windows 原生、系统服务和真实桌面场景仍按第 8 节验收。使用说明见 [README.md](README.md)。

## 1. 概述

### 1.1 目标

xrun 让开发者和 AI Coding Agent 在已授权的 macOS、Windows、Linux 设备上执行命令、传输单个文件、运行构建和测试、截图检查图形效果、取回构建产物。文件在本地由使用方自己的工具读取和编辑。调用方必须显式指定目标设备。

部署形态是一台所有设备都能访问的 **Linux Server**，加上若干台客户端设备。Server 可以有公网地址，也可以只在内网中运行；客户端主动连出，不需要公网地址或入站端口。

xrun 集中做三件事：

1. **便于部署**：Server 上执行 `xrun up`，客户端执行 `xrun join <链接>`，完成初始化、加入和自启动。端口默认 9528，也可以自定义。
2. **便于 AI 调用**：程序和参数直接传递，文件传输、脚本和基础截图有专用接口；正常执行时透传远端输出，用任务引用、退出码和 JSON 查询结果。
3. **任务状态只有一份**：Server 负责身份和转发；任务状态、日志和去重记录全部由目标设备的 daemon 保存。

### 1.2 设计原则

- 同一个逻辑设备持续使用一个 `device_id`；设备名是便于输入的别名，证书是认证凭证。
- 会话断开不影响任务；网络重试沿用原 `request_id`，不生成新的执行请求。
- 结果无法确认时明确返回“未确认”，调用方先查询，再决定是否重新执行。
- 用统一规则处理失败：pull 可以重试，push 在响应丢失后不自动重发。
- 保留启动前持久化、去重、进程清理和资源上限；功能按日常使用需要增加。

### 1.3 本版范围

| 本版包含 | 本版不做 |
| --- | --- |
| Linux Server；macOS、Windows、Linux CLI 和 daemon | macOS、Windows Server |
| `up/join`、基本地址探测、用户级自启动、纯内网部署 | 全面的网卡识别、防火墙诊断和系统睡眠检测 |
| 邀请、来源白名单、撤销、同一设备的证书更新 | 多用户、组织、复杂 RBAC、私钥丢失后的身份恢复、运行中切换 Server 或设备身份 |
| 命令与脚本执行、有界 stdin、流式输出、后台任务、等待、取消 | PTY、端口转发、自动调度 |
| 单个文件 push/pull、原始字节和摘要校验、可选覆盖条件 | 内置文件读取/编辑、目录列表、grep/glob、工作区别名、目录传输和文件同步 |
| 断线重连、日志补读、显式 `request_id` 恢复提交 | 根据“相同命令”猜测重试意图、自动拦截再次执行 |
| 基础截图、基本设备信息和 `guide` | 多显示器选择、按窗口截图、录屏、工具版本自动探测 |
| 自建 Server，通过 TLS 转发 | 托管中转、端到端加密、P2P、Web 控制台、移动端、MCP |

右栏只用于划清范围，不预先设计协议、命令或实现。需要列目录、搜索、打包目录和探测工具时，通过 `exec` 调用远端已有工具；工具未安装时明确报错。

**代码同步通过 Git 完成。** xrun 不自动拉取、切换分支或解决冲突。例如：

```bash
git push origin feat/x
xrun win1 -C 'D:\valle' -- git fetch origin
xrun win1 -C 'D:\valle' -- git switch feat/x
xrun win1 -C 'D:\valle' -- git pull --ff-only
xrun win1 -C 'D:\valle' -- cargo test
```

### 1.4 使用场景

- 在本机驱动另一台设备完成构建和测试，断线后查询结果并补读日志。
- 在三个平台上直接传递参数、输入和脚本，减少远端 Shell 引号与编码差异。
- pull 取回远端文件，在本地使用 AI 自己的工具修改，再 push 回去；构建产物也通过 pull 取回。
- 构建并启动图形程序，截图观察渲染、布局和交互后的画面，再修改代码验证。
- 对网页，通过 exec 运行项目自己的无头浏览器截图脚本，再用 pull 取回图片；无需目标设备有桌面会话。
- daemon 运行在目标用户的会话中，可以使用该用户的工具链、钥匙串、签名工具和模拟器。

## 2. 架构

### 2.1 组成

```text
AI Coding Agent / 开发者
        │
        ▼
      xrun CLI
        │ WSS（mTLS）
        ▼
┌──────── Linux Server，默认端口 9528 ────────┐
│ 身份：CA、设备证书、邀请、撤销、管理设备      │
│ 转发：把 CLI 的会话接到目标 daemon           │
│ 本机 CLI/daemon 使用管理设备身份 admin      │
└────────────────────▲───────────────────────┘
                     │ 设备主动连出
          ┌──────────┼──────────┐
      mac1 daemon win1 daemon linux1 daemon
          任务状态、日志、去重记录在目标设备上
```

| 组件 | 职责 |
| --- | --- |
| Server（`xrun server`） | 设备身份、在线状态、会话转发、连接审计 |
| CLI（`xrun` 的各个命令） | 发起调用、展示输出、保存提交记录、取回的文件和截图 |
| daemon（`xrun daemon`） | 来源授权、执行和进程管理、任务状态、日志、去重、文件传输和截图 |

三个组件由同一个二进制提供。设备上的 CLI 和 daemon 共用身份；只运行 CLI 的设备只能发起调用，运行 daemon 的设备才能被调用。Server 主机也可运行 daemon；Server 和 daemon 是独立进程。

**任务**是一次有持久化状态的执行；**会话**是 CLI 与目标 daemon 之间经 Server 转发的连接。任务不依赖会话。未撤销的目标设备离线时，Server 无法回答它的任务或日志，返回 `DEVICE_OFFLINE`；目标身份已撤销时返回 `DEVICE_REVOKED`。

### 2.2 连接方式

- daemon 保持一条控制连接，上报在线状态和基本设备信息，接收会话请求。
- 每条发往目标 daemon 的 CLI 命令打开一个会话。Server 通知目标 daemon，daemon 检查来源后，为这个会话单独建立一条数据连接。
- Server 原样转发两端的 WebSocket 消息。每个会话独立依靠 TCP 背压；大文件和慢读者不堵住其他会话或控制连接。
- 会话断开后，任务继续运行。CLI 重新打开会话可以查询、等待和补读日志。

本版按会话建立连接，不实现连接复用和应用层多路流控。

### 2.3 部署流程概览

在 Linux Server 上执行：

```bash
xrun up
```

```text
server: 203.0.113.10:9528, 172.31.5.20:9528
xrun join 'xrun://203.0.113.10:9528,172.31.5.20:9528/7q3kxm…#r4t9…'
```

在 Mac 上执行打印出的 `join` 命令，可加 `--name mac1` 指定名称：

```text
mac1 (dev_…)
```

之后可执行 `xrun admin -- uname -a`。命令诊断使用英文，`guide` 输出 README 内容。

### 2.4 信任与授权

- Server 是受信任节点。TLS 保护设备与 Server 之间的连接；Server 主机上的人能够看到或修改会话内容。
- daemon 以启动它的用户身份运行，不提权，不提供沙箱。授权控制一台设备，就授予了该用户可执行命令和访问文件的权限。
- 每个 daemon 在 `daemon.toml` 中保存来源白名单 `allow_from`，以设备 ID 为键，约束执行、文件传输、截图、任务查询、日志和取消。默认拒绝所有来源。
- Server 只认证设备和转发，不保存白名单副本。
- **邀请即互信**：首次注册时，新设备将邀请方加入本机白名单；邀请方的 daemon 在线时，Server 通知它加入新设备。邀请方不在线时，`join` 提示之后执行 `xrun allow-from <新设备>`，Server 不保存待授权队列。
- `invite --no-allow` 只注册、不授权。注册重试、更新地址和证书更新都不产生新的授权。
- `allow-from/deny-from` 在被控设备上执行，参数是调用来源。例如在 win1 执行 `xrun allow-from mac1`，表示允许 mac1 控制 win1。可以手动编辑白名单，也可以经已授权的远程执行调整，几秒内生效；每次热更新都关闭已不允许的来源会话。
- `deny-from` 不终止已启动任务；需要时先取消并确认。管理设备可以用本机命令 `xrun revoke <设备>` 切断该身份的全部连接。

邀请链接通过可信渠道传递。默认邀请意味着对邀请方的控制权。第三台设备与邀请双方之间仍然默认拒绝。

## 3. Linux Server

### 3.1 运行条件

Server 只支持 Linux。默认使用 systemd 用户服务，并启用 linger，使用户退出后和机器重启后继续运行。没有 systemd 时，用 `up --no-service` 在前台运行，由使用方管理进程。

Server 监听默认 TCP 9528，默认绑定 `0.0.0.0`。本版 Server 监听和客户端连接均只支持 IPv4；手动指定的域名必须有 A 记录，客户端只使用其 IPv4 地址。只有 AAAA 记录的域名和纯 IPv6 网络不支持。使用方放行入站端口，xrun 不自动修改防火墙或云安全组。

### 3.2 部署：`xrun up`

1. 生成私有 CA、服务端证书和数据库，保存到 `~/.xrun/server/`；部署配置保存到 `~/.xrun/config.toml`。
2. 启动 Server，默认注册 systemd 用户服务。
3. 在本机通过一次性管理 Token 走普通注册流程，取得管理设备身份；默认同时安装本机 daemon。`--no-daemon` 只注册身份。
4. 打印监听地址、通用端口放行提示和下一台设备的邀请链接。

管理设备的本机注册通过回环地址连接；服务端证书同时包含所用回环地址，不依赖公网连通性。

`up` 可以重复执行：保留 CA、身份和数据库，显示状态并生成新邀请。端口已部署后不在重复执行中隐式修改。`--no-service` 完成初始化后在前台运行 Server；daemon 可用 `--no-daemon` 关闭。

### 3.3 端口与地址

| 参数 | 行为 |
| --- | --- |
| `--port <端口>` | 监听端口，默认 9528 |
| `--addr <主机>:<端口>` | 可重复，将发布地址设为手动模式；提供的地址替换原发布地址列表，不自动探测 |
| `--no-detect` | 在自动模式下禁用公网 IP 查询，只使用本机网卡地址；设置持久化，不改变手动模式的地址 |

首次部署未指定 `--addr` 时使用自动模式，收集非回环、非链路本地的本机 IPv4 地址；默认还向一个公共 IP 查询服务发起 HTTPS 请求，整个查询最多 2 秒，失败就跳过。本版不判断 Docker、VPN 等网卡类型，也不预先判断候选地址是否可达。

`~/.xrun/config.toml` 保存 `manual`（true 为手动，false 为自动）、发布地址和 `no_detect`。Server 重启和省略地址选项的重复 `up` 都沿用保存的模式：手动模式保留原地址，只有显式执行 `up --addr ...` 才替换；自动模式重新探测，并沿用已保存的公网查询设置。手动模式下 IP 改变需要重新执行 `up --addr ...`。

所有发布地址都写入邀请链接和服务端证书 SAN。公网查询取得的是出口 IP，可能不能从外部连入；需要时通过 `--addr` 指定实际地址或映射后的端口。

```text
xrun://203.0.113.10:9528,172.31.5.20:9528/<CA 指纹>#<Token>
xrun://myserver.example.com:443/<CA 指纹>#<Token>
```

客户端逐个尝试地址，每个地址最多等待 5 秒，验证通过的第一个地址胜出。客户端在本地缓存同一 CA 上次验证成功的发布地址；下次 join、CLI 连接或 daemon 重连时，该地址仍在当前地址列表中就先试它，再按原顺序尝试其余地址，同一轮不重复尝试。缓存缺失、损坏或写入失败不影响连接；缓存只优化顺序，每次连接仍须验证指纹、证书和 SAN。全部失败时返回最后的连接错误；可用 status 检查本机状态，并核对发布地址、端口和网络。

按上述模式得到的发布地址改变时，重新签发服务端证书，CA 不变。客户端无法连接时，用同一 Server 的新链接再次 `join`，只更新地址。推荐固定 IP 或域名。

纯内网无需互联网连接：公网查询超时后跳过，或者直接用 `--addr <内网地址>:9528`。

### 3.4 CA、设备 ID 与证书

| 对象 | 规则 |
| --- | --- |
| `device_id` | 首次注册由 Server 生成，在该 Server 内唯一；更新地址和证书保持不变 |
| 设备名 | 唯一的可读别名；认证、白名单、去重和持久化任务引用使用设备 ID |
| 私钥和证书 | 证明设备身份；本版沿用同一把设备私钥更新证书 |
| CA | 20 年有效，不自动轮换；数据丢失或到期时重新部署并重新加入 |
| 服务端证书 | 最长 10 年；启动时使用原服务端私钥重新签发，包含当前发布地址 |
| 设备证书 | 最长 10 年；建立认证连接前，剩余不足 1 年或已过期时用已有私钥更新 |

证书有效期不超过 CA 的有效期。设备证书只用于 `clientAuth`。Server 以设备 ID、登记的公钥和撤销状态认证设备；同一登记公钥的新旧有效证书都属于同一设备，不因更新证书改变白名单、任务或去重记录。

证书更新复用 `POST /pair` 的已登记公钥分支（见 3.5）：客户端使用只验证 Server 的 TLS 连接，提交原私钥签署的证书请求；Server 校验后返回同一设备 ID 的证书。过期证书无需用于这次请求。客户端验证并原子保存新证书。

Server 在握手中发送服务端证书和 CA。客户端固定 CA 公钥的 SHA-256 指纹，以该 CA 验证签名、有效期和 SAN。设备被撤销后，证书更新和所有认证连接都被拒绝。

### 3.5 邀请、注册与撤销

邀请格式为 `xrun://<地址列表>/<CA 指纹>#<Token>`。CA 指纹使用小写 base32，共 52 个字符；Token 是 128 位随机数，base32 编码为 26 个字符，10 分钟有效、一次性使用。Server 只保存 Token 摘要。单 IPv4 地址的链接约 105 个字符。

**注册（`POST /pair`）**

- 校验证书请求的私钥持有证明。
- 公钥尚未登记：验证 Token 和设备名，在同一事务中消费 Token、生成设备 ID、登记公钥和证书，保存首次注册的邀请方及授权结果。
- 公钥已经登记：返回同一设备 ID 的注册结果，不检查或消费 Token，不产生新授权；证书临近到期或过期时更新证书。被撤销时返回 `DEVICE_REVOKED`。
- 名称占用时返回 `NAME_IN_USE`，不消费 Token。证书过期不释放名称，因为原设备可以更新证书；撤销后名称可以复用。
- 配对接口按 TCP 来源 IP 限速，每分钟最多 60 次；不信任代理转发头。注册响应丢失时，用同一把私钥重试，可以取回同一设备身份。

**认证与撤销**

除配对接口外，接口要求有效设备证书；来源 ID 从认证连接取得。无效客户端证书不会降级为匿名认证；配对与证书更新使用不附带客户端证书的连接。不启用 TLS 0-RTT。

管理设备执行本机命令 `xrun revoke <设备>` 后，Server 拒绝该身份的新连接与会话，关闭现有连接，并拒绝证书更新。撤销不保证该设备已经启动的进程停止。

设备 ID 不是恢复凭证。本版不提供私钥丢失、身份撤销后的 ID 恢复，也不热切换身份：需要时先撤销旧身份、清理本机数据，再以新逻辑设备注册。新身份需重新授权，旧设备的任务不会自动迁移。

### 3.6 管理权限

管理设备是 `up` 在 Server 主机注册的身份，例如 admin。管理操作通过已认证 API 执行；本版唯一的管理操作是撤销设备。

只有管理设备身份能执行 `xrun revoke <设备>`，其他身份返回 `NOT_ADMIN`。命令直接访问 Server，不要求被撤销设备在线。已获得 admin 授权的设备也可以远程执行 `xrun admin -- xrun revoke win1`。

管理设备不能通过 API 撤销。其身份丢失时，在 Server 主机重新执行 `up`，通过本地 Server 数据撤销旧管理身份、注册新身份。涉及旧管理 ID 的授权需要重新建立；能够访问 Server 数据目录的账户是最终管理者。

### 3.7 会话转发

- CLI 请求 `WSS /devices/{id}/session`。Server 认证来源，先检查来源和目标的撤销状态，再检查目标在线状态；已撤销目标返回 `DEVICE_REVOKED`，不降级为离线。通过后向目标控制连接发送 `session_request {session_id, source_device_id}`。
- daemon 检查白名单；允许时用自己的证书连接 `WSS /daemon/sessions/{session_id}`，拒绝时回复 `session_reject`。10 秒未接入时返回 `SESSION_UNAVAILABLE`。
- `session_id` 是 128 位随机数，绑定目标设备和当前控制连接，只允许接入一次；CLI 已断开、请求超时或控制连接已被替换时拒绝接入。
- 每台设备只保留最新控制连接。替换控制连接时关闭旧连接、旧会话，并作废尚未接入的请求。
- Server 不解析业务内容。每个方向最多缓冲一条消息，单条消息上限 1 MiB；写入阻塞时停止读取发送方。
- 每个来源最多 16 个并发会话，每个目标最多 32 个。任一端断开时关闭另一端；控制连接断开时关闭该 daemon 的所有会话。
- 5 分钟没有消息的会话关闭；等待和日志跟随用 WebSocket ping 保持连接。

### 3.8 存储与审计

| 位置 | 内容 |
| --- | --- |
| `~/.xrun/config.toml` | 监听端口、地址模式、发布地址、公网 IP 查询设置和 Server 数据目录 |
| `~/.xrun/server/` 下的密钥文件 | CA 和服务端密钥、证书 |
| `server.db` | 设备、公钥、撤销、管理身份、邀请摘要、基本设备信息和连接审计 |
| 服务日志或前台 stderr | Server 运行日志和 TLS 认证失败 |

Server 不保存命令、参数、输出或文件内容。数据库审计记录身份相关事件、会话的来源和目标、时间、持续时间、接收到的消息载荷字节数；TLS 认证失败写入运行日志。

目录权限为 `0700`，私钥为 `0600`；CA 私钥和数据库一起备份。Server 由 systemd 自动重启；开启 linger 失败时，明确打印需要使用方执行的命令。防火墙提示使用通用说明，不检测各发行版的防火墙规则。

## 4. 客户端

### 4.1 加入：`xrun join`

1. 解析链接，尝试地址。先核对 CA 指纹，再验证服务端证书；验证通过前不发送 Token。
2. **先保存私钥，再注册**：生成私钥，连同已核验的 CA 和指纹写入 `pending.toml`，通过临时文件、fsync、原子替换持久化后才发送 `/pair`。证书请求由同一私钥生成；暂存文件属于其他 CA 时明确报错，保留记录。
3. 提交 Token、设备名和证书请求，通过固定 CA 的 TLS 接收注册结果；已有身份时核对返回的设备 ID，再原子保存完整 `identity.toml`。失败时保留暂存文件；成功后删除。
4. 完成首次注册产生的本机授权，安装并启动 daemon。邀请方离线时给出手动授权提示；使用 `status` 和实际调用检查连通性。

完整身份包括私钥、证书、设备 ID、名称、固定 CA、地址，以及首次安装所需的注册结果。身份保存后 daemon 安装失败，重试补完安装；不会注册第二台设备或再次产生授权。首次本机白名单只在创建 `daemon.toml` 时初始化；已有配置保留，不覆盖使用方后续的 `deny-from`。

`--name` 指定首次注册名称，默认按主机名生成。`--no-daemon` 只安装 CLI 身份。

**已有身份时再次加入**

- 同一 CA：更新地址；需要时更新证书，保留设备 ID、白名单、数据库和任务。补完尚未完成的首次安装；服务已运行时不重启 daemon。
- 身份被撤销：返回 `DEVICE_REVOKED`；不自动注册新身份。
- 其他 CA：返回 `DEPLOYMENT_MISMATCH`。本版只使用一个 Server，切换前先明确清理本机数据。

普通地址恢复和证书更新沿用已有设备身份。

### 4.2 daemon 的运行与自启动

| 平台 | 方式 |
| --- | --- |
| Linux | systemd 用户服务；需要 linger 才能退出登录后继续运行 |
| macOS | 用户 LaunchAgent，登录后运行，可以使用登录钥匙串和图形会话中的工具 |
| Windows | 用户登录时的计划任务，运行在交互会话中 |

`join/up` 安装 daemon，也可用 `daemon install/uninstall` 管理服务，或用 `xrun daemon` 在前台运行。每个本机身份只运行一个实例，崩溃后自动重启。daemon 每次建立连接前读取最新身份文件，使用更新后的地址和证书。

首次安装建立数据库；已经初始化后，数据库缺失或损坏时拒绝启动。使用 `daemon reset` 明确重建，旧任务、日志和去重记录丢失，生成新的 `db_id`，设备身份不变。reset 前停止 daemon；运行中的正常停止会取消任务。

daemon 正常退出会取消运行中的任务并持久化结果。升级前应等现有任务结束。

macOS 访问受保护目录仍受 TCC 约束，截图需要“屏幕录制”权限；截图请求缺少权限时明确报错。服务通常不加载交互 Shell 配置，工具链 PATH 放在 `daemon.toml` 的 `[env]` 中。

### 4.3 命令

**指定设备的操作统一把设备放在 xrun 后面：**

```text
xrun <设备> <操作> [参数和选项]
xrun <设备> [执行选项] -- <程序> [程序参数…]
xrun <设备> start [执行选项] -- <程序> [程序参数…]
```

`xrun --help` 和 `xrun --version` 是本机全局选项。其余命令的第一项是本机子命令时，由本机 CLI 处理；否则作为设备名或设备 ID 解析。执行入口中的 `--` 标记远端程序的开始，之后所有参数原样交给程序，不再解析 xrun 选项。没有 `--` 时，只接受已定义的设备操作或显式脚本选项；未知操作返回参数错误。

操作选项放在操作名后，例如 `xrun win1 screenshot --json`。前台执行选项放在设备与 `--` 之间，例如 `xrun win1 -C 'D:\valle' -- cargo build`；后台执行选项放在 start 后，例如 `xrun win1 start -C 'D:\valle' -- 'target\debug\app.exe'`。

常用命令直接写操作意图：执行程序、start 启动后台任务、push/pull 传文件、screenshot 截图。后台任务始终使用实际返回的任务 ID 查询或取消。部署、授权、身份管理和总体状态使用本机命令；执行、任务、文件传输和截图使用设备开头的命令。

**本机命令**

| 命令 | 用途 |
| --- | --- |
| `xrun up [--port N] [--addr 主机:端口] [--no-detect] [--no-service] [--no-daemon]` | 只在 Linux 上部署 Server |
| `xrun join <链接> [--name 名称] [--no-daemon]` | 首次加入，或恢复同一设备的地址和证书 |
| `xrun invite [--no-allow]` | 以本机身份生成邀请 |
| `xrun allow-from <来源设备>`、`xrun deny-from <来源设备>` | 允许或拒绝该来源控制本机 daemon |
| `xrun revoke <设备>` | 本机须为管理设备身份；向 Server 撤销该设备，不要求目标在线 |
| `xrun status` | 显示本机状态和已加入的设备列表 |
| `xrun recent` | 列出本机最近提交记录 |
| `xrun down [--purge]` | 停止并移除本机服务；purge 经交互确认后删除本机全部 xrun 数据 |
| `xrun server` | 前台运行 Server，只支持 Linux |
| `xrun daemon [install\|uninstall\|reset]` | 无子命令时前台运行；子命令安装、移除服务或重建任务数据库 |
| `xrun guide` | 输出 README 使用说明 |

**指定设备的操作**

| 命令 | 用途 |
| --- | --- |
| `xrun <设备> [选项] -- <程序> [参数…]` | 执行远端程序 |
| `xrun <设备> start [选项] -- <程序> [参数…]` | 启动后台任务，受理后返回任务引用；默认不限时 |
| `xrun <设备> --script <shell> [选项] [-- 参数…]` | 从 stdin 执行远端脚本，Shell 必填 |
| `xrun <设备> info` | 查询该设备的基本信息，离线时也可用 |
| `xrun <设备> jobs [任务 ID] [--running] [--request-id ID]` | 无 ID 时列任务，有 ID 时查该任务详情；列表过滤选项只用于无 ID 的查询 |
| `xrun <设备> wait <任务 ID> [--timeout 秒] [--tail N]` | 等待结果 |
| `xrun <设备> logs <任务 ID> [--tail N] [--after 序号] [--follow]` | 查询日志 |
| `xrun <设备> kill <任务 ID>` | 取消并等待确认 |
| `xrun <设备> push <本地源> <远端目标> [选项]` | 上传单个文件；本地源为 - 时读取 stdin |
| `xrun <设备> pull <远端源> [本地目标] [选项]` | 下载单个文件；本地目标为 - 时输出原始字节 |
| `xrun <设备> screenshot [本地路径]` | 拍摄主显示器，保存 PNG，输出本地路径；省略路径时保存到临时目录 |

status 的设备列表、info 和本机 revoke 由 Server 回答或处理；执行、任务、文件传输和截图操作由目标 daemon 处理。本机 allow-from/deny-from 的参数是来源身份；修改远端白名单可显式执行 `xrun win1 -- xrun allow-from mac1`。

```bash
xrun win1 -- cargo test
xrun win1 pull 'D:\valle\src\main.rs' ./main.rs
xrun win1 screenshot
xrun win1 wait k3m9x2
```

Server 主机执行 `down --purge` 会删除 CA 和注册数据，所有设备需要重新加入。普通客户端 purge 后视为新设备；清理数据前先确认未结束和未确认的任务。

### 4.4 设备与任务引用

设备可以用名称或 `device_id` 指定。执行、任务、文件传输和截图的目标设备位于 xrun 后面；revoke、allow-from、deny-from 则以本机命令后的参数指定相关身份。文件路径是独立参数，例如 `xrun win1 pull 'D:\valle\a.rs' ./a.rs`，无需携带设备前缀，也不按 `:` 拆分。Shell 中的反斜杠路径应加引号。

设备名格式为 `[a-z][a-z0-9-]{0,31}`，不得使用本机子命令名或设备操作名。默认由主机名转小写、替换非法字符并截断；结果不合法时使用平台名加 1（例如 macos1），名称占用时通过 `--name` 指定。本版不提供改名命令。

任务短 ID 由 daemon 生成，为 6 个 Crockford base32 字符，在数据库内唯一，冲突时重新生成。人读引用为 `<设备名>/<短 ID>`，稳定引用为 `<device_id>/<短 ID>`。JSON 和本机提交记录同时保存设备 ID 和任务短 ID。

指定设备后，任务参数通常只写短 ID，例如 `xrun win1 logs k3m9x2`。也接受完整任务引用，方便复制；完整引用中的设备必须与前面指定的设备解析为同一 ID，否则返回参数错误，不转发到其他设备。

`jobs` 不带 ID 时列出任务，带 ID 时返回该任务详情。`--running` 和 `--request-id` 只用于列表查询，与任务 ID 同时提供时返回参数错误。

更新地址或证书后，稳定引用仍然有效。名称被其他设备复用时，按名称访问的是当前设备；连接重试和 `--request-id` 重试都使用记录中的设备 ID。查询已撤销身份的旧任务时返回 `DEVICE_REVOKED`；未撤销身份的 daemon 离线时返回 `DEVICE_OFFLINE`。两者都不会路由到同名的新设备。

### 4.5 执行选项

| 选项 | 语义 |
| --- | --- |
| `-C <目录>` | 远端绝对工作目录；省略使用 daemon 默认目录 |
| `--env KEY=VALUE` | 可重复，覆盖远端环境变量 |
| `--stdin` | 从本地 stdin 读到结尾，最多 1 MiB |
| `--script <shell>` | 从 stdin 读取脚本，最多 1 MiB；必须指定 Shell |
| `--timeout <秒>` | 从进程启动计时；前台默认 1800，start 默认 0（不限时）；显式指定时覆盖默认值 |
| `--request-id <ID>` | 明确重试同一提交；省略时生成新的 ID |

普通执行等待任务结束并透传输出；start 在任务受理后返回，stdout 输出任务引用。start 成功表示受理，不表示执行成功。需要限制后台任务时长时，显式指定超时，例如 `xrun win1 start --timeout 600 -C 'D:\valle' -- cargo build`。普通再次调用生成新请求，不按命令内容判断是否重复。

`request_id` 由 CLI 自动生成并在发送前持久化，日常调用不需要先运行 UUID 工具或手动提供 ID；`--request-id` 用于显式恢复同一次提交。构建和测试通常前台执行，启动需要继续交互或截图的程序时使用 start。后台提交返回任务引用，后续命令直接使用其中的短 ID；常用示例不要求用 Shell 临时变量保存它。自动化调用可以通过 `start --json` 读取任务 ID。

两种执行入口使用同一套任务和 exec 协议。start 也接受 stdin 和脚本选项，例如 `xrun linux1 start --script sh < ./serve.sh`；它只改变 CLI 是否等待，以及未指定超时时的默认值。CLI 先解析出实际执行参数，再持久化提交记录并提交。

### 4.6 输出、退出码与 JSON

前台执行按原始字节分别透传远端 stdout 和 stderr。远端程序正常结束时，xrun 不额外输出内容；错误或需要后续确认时在 stderr 打印 `[xrun] <说明>`，附上任务引用或 `request_id`。

| 退出码 | 含义 |
| --- | --- |
| 0–255 | 远端程序的退出码 |
| 1 | Windows 上超出 255 的退出码；完整值写入诊断信息和任务结果 |
| 128+N | 远端进程被信号 N 终止 |
| 124 | 超时，已确认受管理进程停止 |
| 130 | 本地 Ctrl+C，已确认取消或没有受理任务 |
| 75 | 已受理或可能送达的执行、修改或取消结果未确认，以及等待已有任务结果超时或无法确认 |
| 125 | 请求发送前的连接失败、明确拒绝、执行层错误或任务结果丢失等 |
| 2 | 本地参数错误 |

输出不完整时保留远端退出码，同时注明 `TRUNCATED`、`DETACHED_OUTPUT` 或 `CAPTURE_ERROR`。

其他命令成功为 0，已确认的文件等操作失败为 1，连接或执行层错误为 125，本地参数错误为 2。`jobs` 查询成功返回 0，不受任务结果影响；无任务时返回空列表。

连接与提交失败按当前操作所处阶段处理：

- 执行提交、push 或 kill 尚未开始发送时，连不上 Server、目标离线等连接错误返回 125；本次提交、上传或取消请求收到连接、身份、版本或并发检查的明确拒绝也返回 125。此时该操作没有被受理，可以重试；`DEVICE_BUSY` 可以稍后沿用原 request_id 重试。
- 上述请求可能已经送达、但未收到确定结果时返回 75。CLI 记录“请求是否可能已发出”，在开始写入请求前设置；写入报错也不能据此认定未发送。收到任务引用后，执行结果仍需等任务结束才能确认。只有收到明确的未受理响应，才可以把这次请求判为拒绝。
- jobs/logs、pull 和截图等只读操作的连接失败返回 125，可安全重试。wait 在等待已有任务结果时，超时、连接无法恢复或目标离线仍返回 75；明确的身份、撤销或版本拒绝返回 125。wait 的 75 不表示提交了新任务，也不能据此重新执行原命令。

125 本身不证明原任务从未执行；`RESULT_LOST`、`DB_RESET` 等仍需先确认原任务。可以直接重试的范围是这次明确未发送或未受理的操作。连接恢复时的新会话被拒绝，不能推翻原操作已受理或可能送达的状态；原操作仍按结果未确认返回 75。执行提交的本机中断恢复仍按原 request_id 查询，不把缺少发送确认当成未执行。

远端程序本身也可能输出诊断前缀或返回相同的退出码。自动化需要可靠状态时，用 `xrun <设备> start --json [选项] -- <程序> [参数…]` 提交，再用 `xrun <设备> wait <任务 ID> --json` 查询；构建和测试需要限制时长时，在 start 上显式指定 `--timeout`。

`--json` 用于状态、设备、任务、文件传输和截图结果，以及 start 提交；前台执行不支持 JSON。采用固定字段，没有精简/verbose 两套格式。`pull … -` 输出文件原始字节，与 `--json` 互斥；`push - … --json` 可以使用，stdin 是上传内容，stdout 是结果 JSON。

start 和 jobs 的 JSON 返回完整任务对象，包括 `job_id`、来源/目标设备 ID、`db_id`、`request_id`、状态、退出码、持续时间和输出完整性。wait 返回 `{"job":任务对象,"logs":日志事件数组,"logs_error":null或错误说明}`；已确认任务结果后，补读日志失败不改变任务退出码。logs 的 JSON 每行返回一组日志事件。

### 4.7 信号、断线与 AI 使用

- Ctrl+C 在执行请求尚未发送时直接返回 130，不创建任务。请求可能已发送时，先查询受理结果再请求取消，最多等待 10 秒；未能确认时返回 75，打印原 request_id。确认取消或确认没有受理时返回 130。
- SIGTERM、SIGHUP 不取消远端任务；执行请求已受理或可能已发送时，打印任务引用或 request_id，返回 75；明确尚未发送时返回 125。
- 执行会话在提交后连接丢失时，最多花 30 秒重新打开会话，按日志序号补读。仍无法恢复时返回 75；任务继续运行。提交前的连接失败按 4.6 返回 125。
- 收到执行或取消的 75 后先查询：已知任务引用时用 `xrun <设备> jobs <任务 ID>`，也可以 wait 或 logs；只有 request_id 时用 `xrun <设备> jobs --request-id <ID>`。没有 ID 时先看 `xrun recent`。push 的未确认按 4.10 处理。
- 要恢复同一次提交，重新提供原参数和输入，并显式传入原 `--request-id`；要再次执行，发起新请求。
- 验证图形效果时，用 start 启动程序，通过日志或应用自己的就绪信号确认窗口已显示，再调用 screenshot；CLI 返回的本地路径可直接交给 AI 看图工具。
- 验证网页时，通过 exec 运行无头浏览器截图脚本，用 pull 取回生成的 PNG；浏览器、依赖和页面就绪由项目脚本处理。
- `guide` 直接输出 README，包含这些规则、常用命令、目标路径、超时与 Windows 注意事项；使用说明统一维护在 README。

AI 应把“执行未确认”与“已失败”区分开。xrun 不根据相同命令自动阻止 AI 再次执行；使用方必须遵守先查询的规则。

### 4.8 程序、脚本、目录和输入

请求传递 `program` 和 `args[]`，daemon 直接创建进程，不拼接 Shell 字符串。

- 绝对程序路径直接使用；带目录分隔符的相对路径按工作目录解析；只有程序名时按最终 PATH 查找。忽略空 PATH 项，相对 PATH 项按工作目录解析。
- Windows 按 PATHEXT 查找；找到批处理时返回 `SHELL_REQUIRED`，由调用方显式选择 `--script cmd` 或 `cmd.exe /C`。参数编码与长度检查由平台层处理。
- 默认工作目录是用户主目录，可配置；不存在时返回 `INVALID_CWD`。路径不展开 `~` 或环境变量。
- 环境按 daemon 启动环境、`daemon.toml [env]`、请求 `--env` 依次覆盖。凭证不通过环境变量传给任务。

`--script` 将内容写入私有临时文件，用指定 Shell 执行，结束后删除；脚本原文不写入数据库或审计。

| Shell | 文件与执行 |
| --- | --- |
| `powershell` | UTF-8 BOM；`powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File` |
| `pwsh` | UTF-8；`pwsh -NoProfile -NonInteractive -File` |
| `cmd` | CRLF，只保证 ASCII；`cmd.exe /D /C` |
| `sh/bash/zsh` | 原样；使用所选 Shell 执行 |

`--script` 必须显式指定上述 Shell，不按目标平台选择默认值；缺少名称返回参数错误。未安装所选 Shell 返回 `SHELL_NOT_FOUND`。脚本参数写在 `--` 后面；脚本与 `--stdin` 互斥。

进程默认 stdin 为 EOF。`--stdin` 要求管道或重定向，CLI 先完整读取最多 1 MiB；超限或失败时不提交。daemon 收齐后才启动任务，写 stdin 与读取输出同时进行。进程提前关闭 stdin 时以进程结果为准，其他输入写入错误取消任务并报告 `STDIN_IO_ERROR`。输入可包含 NUL 和非 UTF-8 字节。

### 4.9 进程管理与超时

- Unix 每个任务使用独立进程组；Windows 使用独立 Job Object，在创建进程时关联，并设置 `KILL_ON_JOB_CLOSE`。关联失败时不创建不受管理的进程。
- Unix 取消先 SIGTERM，5 秒后仍未退出再 SIGKILL；Windows 使用 `TerminateJobObject`，没有优雅退出宽限期。
- 主进程退出后清理受管理的残留进程，最多再花 2 秒读取输出。脱离进程组却继承输出管道的后台进程不会卡住任务：结束读取并标记 `DETACHED_OUTPUT`。
- 默认最多 4 个 starting/running 任务，满时返回 `DEVICE_BUSY`，不排队、不创建任务、不写去重记录；释放名额后可以用原 request_id 重试。
- 超时包含系统睡眠，不受系统时间调整影响。Linux 用 `CLOCK_BOOTTIME`，macOS 用 `mach_continuous_time`，Windows 用 `GetTickCount64`。
- 至少每秒检查截止时间。设备恢复后发现超时，立即执行终止；睡眠期间不唤醒设备。网络断开不影响计时。

脱离 Unix 进程组的进程不受管理；取消和超时的保证限于受管理的进程。终止失败时不能提前报告已取消或已超时。

### 4.10 单个文件传输

push、pull 和截图请求由 daemon 直接处理，不创建任务，使用相同来源白名单，共享每台设备最多 8 个请求的并发上限。文件传输只处理单个文件，内容为原始字节，不解析文本、编码、换行或编辑指令。

远端路径可以绝对或相对于 `-C`、daemon 默认目录；相对路径用 `/`。本版只接受 UTF-8 路径。远端路径跟随符号链接；push 写入链接目标，保留链接本身。文件源和已存在的目标必须是普通文件，符号链接按目标类型检查；目录返回 `IS_DIRECTORY`，其他不支持的文件类型返回 `INVALID_PATH`。push 的远端目标可以尚不存在。

| 命令 | 行为 |
| --- | --- |
| `xrun <设备> push <本地源> <远端目标> [-C 目录] [--expect sha256] [--mkdir] [--no-overwrite] [--json]` | 上传最多 64 MiB 的单个文件；本地源为 - 时读取 stdin；校验完成后原子写入，成功输出远端绝对路径 |
| `xrun <设备> pull <远端源> [本地目标] [-C 目录] [--json]` | 下载最多 64 MiB 的单个文件；校验完成后保存，stdout 输出本地绝对路径；本地目标为 - 时输出原始字节 |

路径按**源、目标**排列。push 的两条路径都必填；pull 省略本地目标时保存到系统临时目录中的唯一文件。本地相对路径以 CLI 当前目录为准，`-C` 只影响远端路径。目标按文件路径处理，不自动拼接文件名；创建远端父目录用 `--mkdir`，本地父目录需已存在。

```bash
xrun win1 push ./config.json 'D:\valle\config.json'
xrun win1 pull 'D:\valle\artifacts\page.png' ./page.png
xrun linux1 pull /repo/config.json -
printf '%s\n' 'hello' | xrun linux1 push - /repo/note.txt
```

这里的 - 只用于 push 的本地源或 pull 的本地目标。push 从 stdin 读取时，CLI 先将完整输入保存到私有临时文件，最多 64 MiB；输入失败或超限不提交。pull 输出 stdout 前也先完成下载与校验。`pull ... - --json` 返回参数错误；`push - ... --json` 可以使用。

pull 的 JSON 返回 `path`（本地绝对路径）、`device_id`、`remote_path`、`size` 和 `sha256`；push 的 JSON 返回 `device_id`、`remote_path`、`size` 和 `sha256`。摘要对应实际传输的字节，为完整 SHA-256。JSON 不混入路径文本或文件内容。

`push --expect` 是可选覆盖条件，使用修改前从远端下载的摘要，接受 64 个十六进制字符。不匹配或目标已不存在时返回 `STALE`，不覆盖文件。不带时正常创建或替换。`--no-overwrite` 使用操作系统“目标存在则失败”的原子操作，存在时返回 `ALREADY_EXISTS`。这两个覆盖条件互斥，同时提供时返回参数错误。

push 收齐并校验内容后，在目标同目录写入临时文件、fsync，再原子替换。保留远端已有目标的权限；Windows 保留属性和 ACL。不复制本地源文件的权限，也不转换内容。daemon 按解析后的真实路径串行处理 push；带 --expect 时在锁内比较目标摘要，并在替换前再次检查，变化时返回 `STALE`。最后检查与替换之间仍有窗口，不提供与外部程序的严格互斥。目标被占用、无法替换时返回 `FILE_BUSY`。

**pull 可以自动重试；push 在响应丢失后不自动重发，也不根据当前哈希推断此前是否成功。** CLI 返回 75，提示再次 pull 确认当前内容。daemon 尚未提交的上传删除临时文件；已经原子替换的内容不会回滚。

pull 在本地写入临时文件，完整性校验通过后才替换指定目标；中断只删除临时文件，保留已有目标。输出到 stdout 前也先完成校验。

### 4.11 本地修改与目录查询

修改文件采用 pull → 本地编辑 → push。例如：

```bash
xrun win1 pull 'D:\valle\src\main.rs' ./main.rs --json
# 使用本地 AI 编辑工具修改 main.rs
xrun win1 push ./main.rs 'D:\valle\src\main.rs' --expect '<sha256>'
```

`<sha256>` 替换为 pull JSON 中返回的原文件摘要；单人使用时可以不带 --expect。xrun 保持传输字节不变，CRLF、BOM 和文本编码由本地编辑工具负责保留。

列目录通过 exec 调用目标设备已有工具，作为普通任务执行，例如：

```bash
xrun linux1 -- ls -la /repo
xrun win1 -- cmd.exe /D /C dir 'D:\valle'
xrun win1 -- powershell.exe -NoProfile -Command "Get-ChildItem -LiteralPath 'D:\valle'"
```

### 4.12 基础截图

图形验证有两条路径：

- **桌面程序**：通过 `xrun <设备> screenshot` 拍摄主显示器当前画面。
- **网页或无头渲染**：通过 exec 运行项目的截图脚本，脚本生成图片后用 pull 取回。xrun 不提供独立的浏览器截图接口。

例如，项目已有无头浏览器脚本 `scripts/screenshot.js`，输出 `artifacts/page.png`：

```bash
xrun linux1 -C /repo -- node scripts/screenshot.js
xrun linux1 pull /repo/artifacts/page.png ./page.png
```

页面地址、视口、交互步骤和截图时机由项目脚本决定；xrun 负责执行、返回状态和传输图片。这条路径可以在没有桌面的 Linux 设备上使用。

**桌面截图命令**

```bash
xrun win1 screenshot ./screen.png
```

daemon 拍摄当前用户图形会话中的主显示器，按原始分辨率编码为 PNG，通过文件传输协议发送。CLI 校验完整摘要后保存到指定本地文件；省略路径时保存到系统临时目录中的唯一文件。stdout 只输出本地绝对路径，AI 看图工具直接打开该路径。本地相对路径以 CLI 当前目录为准；目标路径、临时写入、校验后替换和中断清理沿用 pull 的文件保存规则。截图仅接受本地文件目标。

`--json` 返回 `path`、`device_id`、`width`、`height` 和 `captured_at`；截图时间采用目标设备的 UTC 时间。PNG 最多 64 MiB。截图不创建任务，不保存到任务日志；审计只记录来源、时间、操作、字节数和结果，不保存图片内容。

调用方决定何时拍摄当前画面。截图与任务分别调用，不自动等待程序退出或判断界面是否就绪。例如：

```bash
xrun win1 -C 'D:\valle' -- cargo build
xrun win1 start -C 'D:\valle' -- 'target\debug\app.exe'
```

后台提交输出任务引用，例如：

```text
win1/k3m9x2
```

后续使用实际返回的短 ID；这里的 `k3m9x2` 仅为示例。先查看日志，确认窗口已显示后截图，检查完成再结束程序：

```bash
xrun win1 logs k3m9x2 --tail 30
xrun win1 screenshot ./screen.png
xrun win1 kill k3m9x2
```

只读截图在连接中断后可以重试，返回重试时重新拍摄的画面，以 `captured_at` 标明时间。

| 平台 | 首版要求 |
| --- | --- |
| macOS | daemon 在用户登录会话中运行；缺少屏幕录制权限时返回 SCREEN_PERMISSION_REQUIRED |
| Windows | daemon 在用户的交互会话中运行；锁屏时返回 SCREEN_LOCKED |
| Linux | 支持 X11 图形会话；Wayland 本版返回 SCREENSHOT_UNAVAILABLE；没有图形会话时返回 NO_DISPLAY |

其他采集失败返回 SCREENSHOT_UNAVAILABLE。基础截图只包含主显示器的当前画面；显示器或窗口选择、自动缩放、裁剪和录屏不进入本版。

### 4.13 本机状态、设备信息与存储

`xrun status` 在同一个输出中显示本机状态和 Server 返回的设备列表。本机部分包含是否加入、身份、版本、服务是否安装和本机实例是否运行，不依赖远端 daemon。实例运行状态通过文件锁判断，包括前台启动的实例。未加入时只显示本机状态，返回 0；已加入但设备列表查询失败时，仍显示本机状态，注明设备列表不可用并返回 125，不把设备全部标成离线。JSON 包含 `local`、`devices`、`server_error`；未加入或列表不可用时 devices 为 null，列表查询失败时 server_error 包含错误码和说明，其他情况下为 null。

status 的设备列表和 `xrun <设备> info` 使用 Server 返回的最后上报信息：`device_id`、`name`、`os`、`arch`、`hostname`、`daemon_version`、`execution_user`、`default_cwd`、`admin`、`online`、`last_seen`。未上报字段为 null；last_seen 是 UTC Unix 毫秒时间戳。离线信息注明最后上报时间，不表示当前状态。

本版不在后台枚举工具或查询工具版本。需要查看 PATH 和工具时通过 exec 调用远端程序。

| 位置 | 内容 |
| --- | --- |
| `~/.xrun/identity.toml` | 私钥、证书、设备 ID、名称、CA、地址及首次注册结果，整体原子保存 |
| `pending.toml` | 首次注册的暂存私钥、CA 和指纹，身份保存后删除 |
| `last-address.json` | CA 指纹和上次验证成功的发布地址，只用于连接顺序；缓存丢失不影响身份和任务 |
| `daemon.toml` | 白名单、默认目录、环境和任务并发上限；日志上限本版固定 |
| `submissions.sqlite` | request_id、来源/目标设备 ID、CA 指纹、db_id、请求摘要、提交时间和任务引用；保留 7 天 |
| `daemon.db` | 启动意图、进程身份、任务状态、去重、本地审计和分块日志；日志序号与内容同一事务保存 |
| 服务日志或前台 stderr | daemon 运行日志 |
| 系统临时目录下的 `xrun-pull-*`、`xrun-screen-*.png` | 未指定目标时取回的文件和截图；由系统清理 |

Windows 使用 `%USERPROFILE%\.xrun`。Unix 私有目录为 0700、私钥为 0600；Windows 使用仅当前账户可访问的 ACL。数据库与日志按敏感数据管理。

本地审计记录来源、时间、程序、参数、目录、任务状态和结果；文件传输记录方向、远端路径、字节数和结果；截图记录时间、来源、字节数和结果。不保存环境变量值、stdin、脚本、文件或图片内容原文。程序自己输出的敏感内容和参数仍可能进入日志，xrun 不自动识别它们。

## 5. 任务语义

### 5.1 状态

daemon 收齐并校验执行请求，检查来源，先按 `(source_device_id, request_id)` 去重。已有请求返回原任务，不受当前并发名额影响；新请求才检查并发、持久化启动意图并创建进程。返回任务引用；前台订阅输出，start 保存并输出任务引用后关闭会话，任务独立运行。

新请求遇到 `DEVICE_BUSY` 时，在持久化启动意图之前直接拒绝，不创建失败任务，也不写去重记录。CLI 收到这一明确拒绝返回 125；稍后用相同 request_id 和参数重试可以正常受理，首次受理后才进入去重规则。

| 状态 | 含义 |
| --- | --- |
| starting | 已持久化启动意图，尚未确认创建进程 |
| running | 主进程已启动 |
| exited | 执行结束，带退出码或信号 |
| failed | 已确认未启动，例如找不到程序或目录无效 |
| canceled | 已确认受管理进程全部停止 |
| timed_out | 超时终止完成，受管理进程全部停止 |
| lost | daemon 崩溃或设备重启，结果无法取得，可能有遗留进程 |

最终状态持久化后不被迟到事件覆盖。存储失败时不能把未持久化结果报告为确定结果；初始记录写入失败时不启动进程。SQLite 使用 WAL 和 FULL 持久化，去重键有唯一约束。

### 5.2 提交、去重与恢复

- CLI 每次普通调用生成新 request_id；连接重试沿用它。
- 会话开始收到 `db_id` 后，发送请求前先持久化本机提交记录；收到任务引用后补全。SIGKILL 也不会丢失已发送提交的 ID。
- daemon 保存规范化请求的 SHA-256：覆盖程序、参数、目录、环境覆盖值、解析默认值后的实际超时、Shell，以及 stdin/脚本长度和摘要。相同 ID、相同摘要返回同一任务；不同摘要返回 `REQUEST_CONFLICT`。去重记录不随日志删除。
- `xrun recent` 只列本机最近 24 小时的提交记录，区分已确认/未确认；明确未发送或已被拒绝的记录注明未受理，不显示为结果未确认。不广播查询设备、不猜测任务当前状态。远端结果通过 `xrun <设备> jobs --request-id <ID>` 查询。
- 显式重试时重新提供同一组参数和输入，传入原 `--request-id`。原请求受理过就返回原任务；未受理就受理一次。
- 本机记录中已有 ID 的重试，校验原 CA、来源设备 ID 和目标设备 ID；不匹配返回 `IDENTITY_CHANGED`，不发送执行请求。更新地址和证书不触发此错误。
- 重试带原 `db_id`；目标数据库已重建时返回 `DB_RESET`，不执行。查询可以查看当前数据库，但查询不到原请求不能解释为此前未执行。
- 提交记录已经删除、或者从其他 CLI 使用未记录过的 ID 时，只保证当前数据库内的去重，无法承诺跨数据库重建的恢复。结果不明时先到目标设备确认。

需要再次执行时使用新 ID；xrun 不承诺任意两次相同命令只执行一次。

### 5.3 断线和重启

| 事件 | 行为 |
| --- | --- |
| 提交前目标离线 | 返回 DEVICE_OFFLINE、退出码 125，不排队，可以重试；已提交任务在断线后无法确认结果时返回 75 |
| CLI 退出或会话断开 | 任务继续运行，可重连查询和补读 |
| daemon 与 Server 断开 | daemon 继续执行、计时和记日志；断线期间无法远程查询 |
| Server 重启 | 会话中断，任务不受影响 |
| daemon 正常退出 | 停止接收请求，取消任务，持久化结果 |
| 存储出错 | 返回 STORAGE_ERROR，不宣称未持久化结果已确定 |

控制连接每 15 秒 ping，45 秒无响应判离线；断线后带抖动指数退避重连，最长间隔 30 秒。撤销或凭证错误停止重连；证书更新依 3.4 处理，不注册新设备。

### 5.4 daemon 崩溃

记录 boot_id、主进程 PID、进程启动时间及进程组或 Job Object 标识。重启后不重跑任务：

- Windows：Job Object 的 KILL_ON_JOB_CLOSE 清理受管理进程。
- Unix 系统已重启：原进程不存在。
- Unix 同次系统启动且能证明进程身份：终止原进程组。
- Unix 无法证明身份：不凭旧 PID 杀进程，标记 `leftover_possible: true`，显示 PID/进程组供人工处理。

未完成任务转为 lost，退出码为 null、错误为 RESULT_LOST，不再占用并发名额。lost 表示结果丢失，不保证遗留进程已停止，也不表示副作用已撤销。

### 5.5 取消、等待和日志

kill 幂等；启动、取消和自然退出按 daemon 的最终持久化状态协调。发送前目标离线、身份被撤销或请求明确被拒绝时返回 125；取消请求可能已送达时最多等待 10 秒确认，未确认返回 75。不排队保存取消请求。

wait 在任务进入最终状态后回复；默认一直等待，`--timeout` 只限制本次等待，不取消任务。结束后输出最后 N 行，默认 40，随后返回任务结果。JSON 模式将尾部日志放入结果字段，不混入额外文本。

输出按字节处理，每块最多 32 KiB，带单调 seq、stdout/stderr 标识和 Base64 数据。同一流内有序，跨流按 daemon 观察的顺序。

daemon 先落本地日志，再通知订阅者。订阅者按自己的速度读磁盘；慢 CLI 不阻塞任务。重连按最后 seq 补读；最终结果包含 last_seq、output_complete、incomplete_reason。

| 限制 | 默认 |
| --- | --- |
| 单任务日志 | 64 MiB，超出继续排空输出但不保存，标记 TRUNCATED |
| 日志总量 | 1 GiB，先删除已结束的旧日志，仍不足则截断 |
| 已结束日志保留 | 7 天；任务和去重记录继续保留 |

logs 默认读取请求时的快照；follow 补读后继续订阅；after 指定序号；tail 取最后 N 行。日志截断提示 LOG_TRUNCATED，已清理提示 LOG_UNAVAILABLE，输出仍可用的日志后返回 1；其他采集失败提示 LOG_INCOMPLETE。目标已撤销时返回 DEVICE_REVOKED，未撤销但离线时返回 DEVICE_OFFLINE，这两种日志查询失败均为退出码 125。磁盘失败要明确报告。

## 6. 协议

### 6.1 Server 接口

所有 HTTPS 请求和 WSS 握手都携带 `X-Xrun-Version`，值为客户端二进制的完整发布版本。Server 在处理业务前与自身发布版本比较，不一致返回 `VERSION_MISMATCH`；配对请求也在消费 Token、登记身份或更新证书前检查。

| 接口 | 认证与职责 |
| --- | --- |
| `POST /pair` | 新公钥用邀请 Token；已登记公钥用私钥持有证明，恢复注册结果或更新证书 |
| `POST /invites` | 设备证书，生成邀请 |
| `GET /devices`、`GET /devices/{id}` | 设备证书，查询基本信息和在线状态 |
| `POST /admin/revoke` | 管理设备证书，撤销 |
| `WSS /devices/{id}/session` | 来源设备证书，打开会话 |
| `WSS /daemon` | 目标设备证书，控制连接 |
| `WSS /daemon/sessions/{session_id}` | 会话绑定的目标证书，数据连接 |

控制消息为 hello、session_request、session_reject 和首次注册产生的 grant；grant 由 daemon 应用后确认，无离线队列。心跳使用 WebSocket ping/pong。

### 6.2 会话消息

daemon 接通后先发送 `{"type":"ready","device_id":"…","db_id":"…","default_cwd":"…","version":"…"}`，version 为 daemon 的完整发布版本；CLI 核对版本和目标 ID 后处理业务。控制连接 hello 中的 version 也使用发布版本。一个会话只处理一个请求，然后关闭。

请求头为 JSON 文本 `{"type":"request","request":{"op":"…",参数…}}`。长度和 SHA-256 在执行或上传请求头中发送；stdin、脚本和文件内容用二进制块，每块不超过 64 KiB，最后发送 `{"type":"end"}`。下载和截图先返回 `file` 头（含长度和摘要），再返回二进制块和 end。

操作为 exec、jobs、wait、logs、kill、push、pull、screenshot。exec 收齐输入、受理后返回 `job`；前台 CLI 随后通过独立的 logs 会话跟随输出和最终状态。logs 返回带任务状态的 `logs` 事件，最后发送 end；wait 和 kill 等待最终持久化状态并返回 job。

CLI 的 start 入口使用 exec，并把解析后的实际超时发给 daemon；jobs 带 ID 时查详情，不带时分页查列表。info 直接访问 Server 的设备接口。push 的本地源（包括 stdin 临时文件）和 pull、screenshot 的本地保存路径只由 CLI 处理，不发送给 daemon。

文件、截图和输入传输结束时校验完整摘要，失败返回 CHECKSUM_MISMATCH，删除未提交的临时文件。格式或顺序错误返回 INVALID_MESSAGE 或 INVALID_BODY 并关闭会话。上传在 daemon 上分块落临时文件；下载先生成私有临时快照，使摘要对应发送的字节。文件和截图请求断线时停止未提交的操作；执行任务继续运行。

### 6.3 版本和限制

CLI、Server 和 daemon 的完整发布版本必须完全一致，使用二进制内同一个版本值，与 `xrun --version` 一致。不单独维护协议版本号，也不按“主版本相同”判断兼容；不一致在业务处理前返回 `VERSION_MISMATCH`。修改消息格式必须随发布版本更新，同一发布版本不得发布不同的消息格式。

升级时 Server 和所有参与调用的 CLI、daemon 都要更新到同一版本，版本不一致的组件不能互通。设备升级前按 4.2 等待现有任务结束。本版不实现版本协商或旧版本兼容层。

单条消息不超过 1 MiB，执行用 stdin/脚本各不超过 1 MiB，push/pull 文件和截图 PNG 不超过 64 MiB；push 从 stdin 读取时也使用文件的 64 MiB 上限。接收时检查，超限拒绝。任务列表分页，CLI 可逐页展示。

程序、参数、环境和路径不得含 NUL；输入与文件原始字节不受此限制。Windows 环境名大小写重复、负数超时、未知字段或类型错误返回 INVALID_REQUEST。

### 6.4 错误码

| 类别 | 错误码 |
| --- | --- |
| 连接和身份 | CONNECT_FAILED、CONNECT_TIMEOUT、INVALID_TOKEN、NAME_IN_USE、DEPLOYMENT_MISMATCH、UNAUTHENTICATED、DEVICE_REVOKED、SOURCE_NOT_ALLOWED、NOT_ADMIN、ADMIN_PROTECTED、DEVICE_OFFLINE、VERSION_MISMATCH、RATE_LIMITED |
| 参数和启动 | INVALID_REQUEST、INVALID_CWD、INVALID_PATH、PROGRAM_NOT_FOUND、SHELL_REQUIRED、SHELL_UNSUPPORTED、DEVICE_BUSY、EXECUTION_ERROR、PORT_IMMUTABLE |
| 输入 | INPUT_TOO_LARGE、INVALID_SCRIPT |
| 任务和存储 | REQUEST_CONFLICT、DB_RESET、DB_MISSING、DB_CORRUPT、IDENTITY_CHANGED、JOB_NOT_FOUND、RESULT_LOST、LOG_TRUNCATED、LOG_UNAVAILABLE、LOG_INCOMPLETE、STORAGE_ERROR |
| 文件 | FILE_NOT_FOUND、PARENT_NOT_FOUND、PERMISSION_DENIED、IS_DIRECTORY、FILE_TOO_LARGE、FILE_BUSY、ALREADY_EXISTS、STALE |
| 截图 | PERMISSION_DENIED、SCREEN_LOCKED、NO_DISPLAY、SCREENSHOT_UNAVAILABLE、SCREENSHOT_FAILED |
| 协议 | INVALID_MESSAGE、INVALID_BODY、CHECKSUM_MISMATCH、MESSAGE_TOO_LARGE、INVALID_SESSION、SESSION_UNAVAILABLE、SESSION_LIMIT |

执行、修改或取消请求可能已送达后的响应丢失不是已确认失败：CLI 用退出码 75 和未确认提示表达。请求发送前失败和已确认拒绝按 4.6 返回 125；只读连接错误、wait 和已有任务结果按该节区分。远端非零退出也不是 xrun 的执行层错误。

## 7. 实现与顺序

单个 Cargo package，按职责组织 server、cli、daemon、session/protocol、transfer、screenshot、service、platform 模块。使用证书、SQLite、进程管理和平台权限代码，不为未来中转平台增加抽象层。

| 步骤 | 工作 | 完成标准 |
| --- | --- | --- |
| 1. 部署和身份 | Linux up、基本地址、join 和暂存恢复、稳定设备 ID、证书更新、邀请/白名单/撤销、自启动 | Linux Server 加任意客户端完成加入和授权 |
| 2. 执行闭环 | Server 会话转发；daemon 状态和去重；提交记录；exec/script/start/jobs/wait/logs/kill；崩溃与输出收尾 | 三平台可靠执行，断线可恢复，不重跑 |
| 3. 文件、截图和接口整理 | 单文件 push/pull、基础 screenshot、统一未确认规则、status/info、JSON 和 guide | AI 完成下载、本地编辑、上传、构建、查询结果、截图检查和取回产物 |

公开 Server 命令始终仅支持 Linux。自动化测试直接调用 Server 库，在每个平台独立覆盖 TLS 转发和当地 CLI/daemon，不开放 macOS/Windows 的 Server 部署入口；实际组合验收仍使用 Linux Server。

当前验证记录：

| 环境 | 已运行的检查 | 尚需验收 |
| --- | --- | --- |
| macOS ARM64 | fmt/clippy；真实 TLS 配对、参数和输入字节、退出码、后台任务、去重、并发、文件、撤销、daemon 崩溃、DB_RESET、日志保留与状态查询 | LaunchAgent、真实桌面/TCC、睡眠和磁盘故障 |
| Linux ARM64 容器 | 上述执行/传输测试；up 前台部署、手动地址保留、CA 稳定和管理身份恢复；Xvfb PNG 截图 | systemd/linger、真实桌面、睡眠和磁盘故障 |
| Windows x86_64 | 交叉类型检查；已配置原生 CI 测试入口 | 原生进程/编码、登录计划任务、截图与锁屏 |

## 8. 验收标准

以下场景需要真实运行证据。协议和任务逻辑可在 Linux CI 验证；平台进程、路径、编码和服务必须在对应系统验证。截图权限、锁屏和图形效果在有图形会话的真实设备上验收。

### 8.1 Linux Server 和身份

| 场景 | 通过条件 |
| --- | --- |
| 默认部署 | Linux up 使用 9528，客户端 join 完成注册、授权和服务安装；自定义端口也可用 |
| 显式地址 | --addr 使用指定地址，不访问公网查询服务；Server 重启和无地址选项的重复 up 保留手动地址；显式 up --addr 才替换；端口映射后链接可连接 |
| 地址尝试顺序 | 同一 CA 的上次成功地址仍在候选列表时优先尝试，失败后依次尝试其他地址且不重复；缓存失效不影响连接；每次仍验证指纹、证书和 SAN |
| IPv4 范围 | IPv4 地址和有 A 记录的域名可用；双栈域名只使用 IPv4；只有 AAAA 记录的域名和纯 IPv6 网络明确不支持 |
| 纯内网 | 查询最多 2 秒后跳过；显式内网地址不依赖互联网 |
| 自动地址设置 | 自动模式重启重新探测；--no-detect 的公网查询设置重启后保留，不覆盖手动地址模式 |
| 地址更新 | 同一 CA 的新链接恢复连接，设备 ID、白名单和任务不变 |
| 首次注册中断 | 注册成功但响应丢失，或身份保存前 SIGKILL；复用暂存私钥、换新链接后取回同一设备 ID，不新增设备、不重复授权 |
| 安装中断 | 身份已保存而 daemon 安装失败；重复 join 补完安装，不再次注册 |
| 证书更新 | 临近到期和已过期都能用登记的私钥取得同一 ID 的新证书；已撤销身份不能更新 |
| 名称与身份 | 证书到期不释放名称；撤销后名称可复用，新设备不能冒充原 ID 或获得旧设备授权 |
| 管理权限 | 普通设备不能撤销；管理设备受保护；本机 up 能修复丢失的管理身份 |
| 转发与绑定 | 错误目标证书、重复/超时接入、控制连接替换后的接入都被拒绝；慢读者和大文件不堵住其他会话 |
| 服务与存储 | linger 生效后重启恢复；Server 数据和日志没有任务内容 |

### 8.2 客户端、文件和截图

| 场景 | 通过条件 |
| --- | --- |
| 连接验证 | 指纹、证书签名、有效期和 SAN 校验；验证通过前不发 Token |
| 授权 | 邀请双方互信；第三方默认拒绝；离线邀请方无待授权队列；allow-from/deny-from 调整本机的调用来源，deny-from 关闭被拒绝来源的现有会话；本机 revoke 须管理身份，不依赖目标在线 |
| 自启动 | Linux linger；macOS 登录 LaunchAgent；Windows 登录计划任务，无额外窗口 |
| 参数与脚本 | 空格、反斜杠、Unicode、空参数保真；--script 必须给出 Shell；PowerShell 中文脚本和 cmd ASCII 脚本正确；批处理不会被隐式执行 |
| 命令组织 | 执行、任务、传输和截图操作先写设备；授权、revoke 和 status 为本机命令；未知操作被拒绝；执行入口 -- 后的参数不被 xrun 解析；路径不拆分冒号；完整任务引用指向其他设备时不转发 |
| 状态与任务查询 | status 合并本机状态和设备列表；Server 不可达仍显示本机状态，列表不可用且返回 125；jobs 无 ID 列表、有 ID 详情，ID 与列表过滤选项互斥 |
| 执行入口 | 前台默认超时 1800 秒，start 默认不限时，显式超时覆盖默认值；start 返回可查询的任务引用；两者复用 exec，去重使用实际执行参数 |
| 输入与输出 | stdin 超限不启动；非 UTF-8 输出原样传递；正常执行没有额外 xrun 输出；JSON 不混入人读内容 |
| 文件传输 | 仅单文件 push/pull，路径按源、目标排列；目录被拒绝；本地相对路径使用 CLI 当前目录，-C 只影响远端；push 保留远端已有目标权限 |
| 字节与管道 | 二进制、CRLF、BOM 和 NUL 原样传输；push - 读取最多 64 MiB，超限不提交；pull 到 stdout 先校验且不能同时 --json；push 从 stdin 可以输出 JSON |
| 覆盖条件 | pull --json 给出下载内容的完整 sha256；push --expect 与远端当前目标比较，不匹配或目标已不存在时不覆盖；不带时正常创建或替换 |
| 并发上传 | 两个内容均不同于原文件的 push 使用同一旧摘要时，只能一个成功；检测到外部修改时拒绝；不声称严格外部互斥 |
| 上传响应丢失 | push 不自动重发，返回 75；再次 pull 能看到实际内容，外部后续修改不会被重试覆盖 |
| 文件完整性 | 未收齐或哈希不符不替换目标；pull 中断不损坏已有本地文件 |
| 看构建产物 | pull 输出的本地路径可直接交给本机工具打开 |
| 图形效果闭环 | 构建并以 start 启动图形程序，确认窗口就绪后截图；本地 PNG 显示预期界面，路径可直接交给 AI 看图工具 |
| 无头网页截图 | 没有桌面的 Linux 设备通过 exec 运行项目的无头浏览器脚本并生成 PNG；pull 取回后能查看网页效果，不依赖桌面截图接口 |
| 截图边界 | 主显示器、原始分辨率；macOS 无授权、Windows 锁屏、Linux 无图形会话和 Wayland 按定义报错 |
| 截图完整性 | 指定本地文件或默认临时路径均在校验后才发布 PNG，中断不损坏已有目标；断线重试返回新截图和新时间；未授权来源不能截图，Server 和审计不保存图片内容 |
| 系统授权 | macOS 受保护目录失败有明确提示；必要的服务权限在目标系统实测 |

### 8.3 任务和故障

| 场景 | 通过条件 |
| --- | --- |
| 执行闭环 | macOS、Windows、Linux 执行、等待、日志和取消都按契约运行 |
| 请求去重 | 同一来源、同一 ID、同一请求返回原任务，即使当前并发已满；更改参数/env/输入/脚本被拒绝 |
| 繁忙重试 | DEVICE_BUSY 返回 125，不创建任务、不写去重记录；释放名额后同一 ID 可以受理，之后继续按原任务去重 |
| 提交恢复 | 发出请求后、收到任务引用前 SIGKILL；recent 保留 ID；原 ID 和原输入重试只执行一次 |
| 提交失败分类 | 请求发送前连接失败或明确拒绝返回 125；开始发送后无法确认受理或执行结果返回 75，发送函数报错不被当作未发送；不把 125 一概解释为未执行 |
| 有意重跑 | 两次普通相同命令产生两次提交；不出现命令匹配拦截 |
| 数据库重建 | 有本机提交记录的旧请求携原 db_id 重试被拒绝，不启动新进程 |
| 引用稳定 | 更新地址和证书后原 device_id/短 ID 仍能查询；连接重试和 --request-id 重试使用记录中的设备 ID，名称复用不改变按 ID 的路由；旧身份已撤销返回 DEVICE_REVOKED，未撤销但离线返回 DEVICE_OFFLINE |
| 网络故障 | 在提交、启动、输出和结束阶段断线；任务不重跑，重连可补读日志 |
| 取消和超时 | 三平台停止受管理子进程；未确认不报成功；wait 超时不取消 |
| daemon 崩溃 | 按进程身份处理遗留任务，转 lost、不重跑、不占并发；不能证明身份时不误杀其他进程 |
| 遗留输出 | 脱离进程组且继承输出的后台进程不让任务永久挂住，2 秒收尾并标记 DETACHED_OUTPUT |
| 睡眠 | 睡眠计入超时，恢复后执行超时检查 |
| 资源与磁盘 | 并发、消息和日志上限生效；慢读不阻塞任务；存储错误不宣称确定结果 |
| 设备离线 | 提交前、只读查询或 kill 未发送时返回 DEVICE_OFFLINE、退出码 125；wait 等待已有任务结果不可确认时仍为 75；不排队执行或取消 |
| 敏感数据 | 数据库和审计无 env 值、stdin、脚本和文件原文；暂存密钥有私有权限 |
| 发布版本 | CLI、Server、daemon 完整发布版本一致；不同版本的 HTTPS、WSS 和 ready 检查在业务处理前拒绝；配对版本不匹配不消费 Token 或登记身份；不单独维护协议版本号 |
| Git 工作流 | 远端 Git 拉取和构建；文件冲突通过 pull、本地编辑、带原摘要 push 解决，再取回产物；目录查询通过 exec |

不为了满足首版验收新增本版范围外的功能；边界条件用上述统一规则处理。
