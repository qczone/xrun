# xrun 设计文档

状态：第一阶段设计，尚未实现。

## 1. 目标与范围

xrun 为开发者和 AI Coding Agent 提供统一的远程命令执行接口，支持在已授权的 macOS、Windows、Linux 设备上查看和修改文件、构建项目、运行测试及执行长任务。

调用方必须显式指定目标设备。xrun 不拦截本地 Shell，不将远端环境伪装成本地环境，不负责选择执行设备。

```bash
xrun ls
xrun info mac1
xrun mac1 -C '/Users/dev/Projects/valle' -- cargo test
xrun win1 -C 'D:\Projects\valle' -- cargo test
```

第一版面向单用户、自托管场景。CLI 和设备 Agent 支持三个操作系统；Server 首先在 Linux 上部署和验收。部署只要求一台具有公网 IP 的服务器和一个可达 TCP 端口，不要求域名、已有 HTTPS 服务或外部证书。

### 1.1 为什么使用 xrun

SSH 本身支持远程命令执行。SSH 配合 Tailscale 或反向隧道，再配合 tmux 和脚本，可以实现本项目的大部分使用场景；已有这套环境、只需偶尔执行命令的用户无需迁移。[OpenSSH 文档](https://man.openbsd.org/ssh)

xrun 将以下行为收敛为三个平台共用的契约：

- 设备主动连接 Server，调用方通过设备身份发现目标，无需为每台目标设备维护入站访问路径。
- 每次执行都有持久化 Job ID，可在断线后查询状态、补读日志，而不依赖终端会话。
- 输入、输出、错误和未知状态可由 Coding Agent 直接解析。
- 超时、取消、去重和重连行为统一定义，平台无法一致的部分明确暴露。

自建连接与任务层的成本是证书管理、两端任务记录和状态核对。第一版以这些契约的真实跨平台验收证明其价值，不以替代 SSH 全部功能为目标。

### 1.2 范围

| 第一版包含 | 第一版不包含 |
| --- | --- |
| 设备配对、发现、撤销、本地来源白名单 | 多用户、组织、复杂 RBAC |
| 显式目标、工作目录、环境变量和超时 | 自动调度、自动选择执行环境 |
| 有界 stdin、流式输出、后台任务、查询和取消 | 交互终端、PTY、端口转发 |
| 断线重连、任务状态核对、日志补读 | Agent 崩溃后的进程接管 |
| 基础审计、结构化 CLI 输出 | Web Console、移动端、MCP |
| 经 Server 转发的加密连接 | P2P、托管服务、端到端加密 |

**代码通过 Git 分支同步，由使用方安排。** xrun 可以执行 `git fetch`、`git switch`、`git diff` 等命令，但不管理分支、同步策略、工作区快照或冲突。文件同步、文件传输及 `xrun push / xrun pull` 命令均不属于第一版；查看和修改文件通过远程命令完成。

## 2. 架构

```text
开发者 / Coding Agent
          │
       xrun CLI
          │ HTTPS / WSS
          ▼
     xrun Server
     ├── 设备注册与认证
     ├── 请求路由与任务索引
     └── 审计
          ▲
          │ 设备主动建立 WSS 长连接
     ┌────┼─────┐
   macOS Windows Linux
     └── xrun agent ── 本地进程
```

| 组件 | 职责 |
| --- | --- |
| CLI | 解析参数、认证、提交命令、展示输出、查询和取消任务 |
| Server | 验证身份和授权、维护设备在线状态、持久化任务索引、路由消息、记录审计 |
| Agent | 检查执行参数、启动和管理进程、保存执行状态与日志、向 Server 上报 |

执行状态以目标 Agent 的持久化记录为准；Server 保存可查询的状态副本。完整日志保存在目标设备，Server 转发并缓存已收到的有限末尾输出，缓存范围按第 7 节报告。

每台设备使用同一设备身份运行 CLI 和 Agent。仅运行 CLI 的设备可以发起调用；只有运行 Agent 的设备才能接受执行请求。Server 所在机器也必须配对并运行 Agent，才能作为 `cloud1` 被调用。

目标设备不监听公网端口。Server 是唯一需要对外提供服务的节点；所有执行流量经过 Server。

## 3. 身份与信任模型

### 3.1 授权边界

- 配对建立设备身份，不自动获得所有目标的执行权限。每个 Agent 在本地配置 `allow_from`，使用不可变的来源设备 ID，默认空列表即拒绝远程调用。
- 白名单同时约束执行、任务和日志读取、取消。Agent 在启动和处理请求时检查本地配置；Server 按目标 Agent 上报的配置检查路由与缓存访问，未取得配置时拒绝访问。
- Server 是受信任节点，可以看到命令、环境变量和输出，并拥有向设备下发命令的能力。
- Agent 以启动它的系统用户身份运行，不要求 root 或管理员权限，不自动提权，也不提供沙箱隔离。
- 任意命令执行包含该系统用户已有的文件读写和网络访问能力，不能用禁用文件接口来限制这些权限。
- 配对和撤销通过 Server 主机上的本地管理接口完成，不向普通设备连接开放管理 API。

```toml
# 目标设备的 Agent 配置：仅接受 cloud1 的设备 ID
allow_from = ["dev_cloud1"]
```

设备名更换或复用不改变白名单。配置在 Agent 启动时读取；更新需重启 Agent 并通知 Server，已失去权限的日志订阅关闭。离线缓存使用最后确认的白名单，不能推测离线配置变化。

Coding Agent 可能受提示注入影响。白名单限制受影响来源能触达的设备，但不能判断命令意图，也不能隔离已获授权的来源。来源身份由受信任 Server 转发；Server 被攻破仍可伪造来源。任意执行还可能修改运行账户可写的配置，因此本机制不是对已授权调用方的沙箱。

### 3.2 连接认证

使用 TLS 保护所有网络连接，CLI 和 Agent 通过设备证书完成双向 TLS 认证。采用标准 TLS 实现，不设计逐条命令签名协议。执行接口禁用 TLS 0-RTT；应用重试由第 6 节的请求去重处理。

Server 首次启动时自动生成私有 CA 和服务端密钥，签发包含公开 IP SAN 及 `serverAuth` 用途的服务端证书，直接提供 `https://<公网IP>:7443` 和 WSS。证书由 xrun 管理，无需 ACME 或预先配置 HTTPS。

配对链接同时携带 Token 和 CA 公钥的 SHA-256 SPKI 指纹。CLI 首次连接时，将服务端提供的根证书与链接指纹核对，再以该根为信任锚完成证书链、IP SAN、有效期和 TLS 握手签名校验；校验完成前不发送 Token 或业务数据。禁止明文引导、跳过校验和默默接受首次见到的证书。

配对链接必须经管理员已有的可信渠道交给设备。攻击者若替换整个链接，就能替换信任锚；指纹不能解决链接本身不可信的问题。配对后本地保存 CA 证书、指纹及 Server 地址，后续不依赖系统信任链；恢复链接不得覆盖已有 CA 固定值。服务端叶证书在同一 CA 下自动续期；更换 CA 必须显式重新建立信任。

同一 CA 签发用途限定为 `clientAuth` 的设备证书。设备生成自己的私钥和证书请求，Server 登记公钥、证书标识与 `device_id` 的关联。私钥不上传；服务端证书不能用作设备身份。

除配对接口外，网络 API 均要求有效设备证书。每次请求及每条具有操作效果的长连接消息都检查证书有效期和设备撤销状态；不能只在 TLS 握手时检查。身份从认证连接取得，不接受请求体自报的 `source_device_id`。

配对入口允许不提供客户端证书，但仍须验证服务端证书；提供了无效客户端证书的连接不得降级为匿名连接。使用 rustls 的可选客户端证书验证，并在路由层强制业务接口认证。[rustls 文档](https://docs.rs/rustls/latest/rustls/server/struct.WebPkiClientVerifier.html)

### 3.3 配对

```bash
# Server 主机：每台设备分别生成一个链接
xrun pair

# 设备端：注册身份后，显式启动 Agent
xrun join 'https://203.0.113.10:7443/pair#token=<token>&ca=<sha256-spki>' --name mac1
xrun agent
```

1. Server 生成密码学安全的随机 Token，熵至少 128 位，有效期 10 分钟；数据库只保存摘要。
2. CLI 先在本地保存待配对私钥和证书请求，再按链接指纹验证 Server，通过 HTTPS 请求体提交 Token、设备名和已签名的证书请求；Token 不进入 URL 路径或查询参数。
3. Server 验证 Token、设备名唯一性及私钥持有证明，在事务中消费 Token 并注册设备。
4. CLI 保存 CA、Server 地址、设备 ID、私钥和证书，显示设备 ID 供目标配置白名单。`join` 不自动安装服务或启动后台进程。

一个 Token 只绑定一次注册。同一个 Token 与同一证书请求在有效期内重试时返回同一注册结果；不同公钥的重用被拒绝。该规则用于恢复配对响应丢失，不允许一个 Token 注册多台设备。

设备名是唯一别名，设备 ID 是不可变标识。已存在的名称不能被另一把密钥覆盖。设备证书默认有效期 90 天；CLI 或 Agent 在到期前通过已认证连接为同一设备和公钥续期。

长期离线导致证书过期时，管理员执行 `xrun pair --renew <device>` 生成绑定原设备 ID 和公钥的恢复链接。设备用原私钥证明持有权后取得新证书，保留原身份；不能更换公钥或恢复已撤销设备。此流程只使用服务端 TLS 验证与恢复 Token，不提交过期客户端证书。私钥丢失则必须撤销后重新注册。

### 3.4 撤销

```bash
# 在 Server 主机执行
xrun revoke mac1
```

撤销写入数据库后，立即拒绝该身份的新请求、取消尚未下发的请求，并关闭其现有 CLI 与 Agent 连接；证书续期不能恢复被撤销的身份。设备重新加入需要新的配对授权和设备 ID，名称可在旧身份撤销后复用。

撤销控制的是后续访问，不承诺终止已经启动的进程。需要停止任务时，应先取消并确认结果；离线设备上的进程只能在设备本地处理。

## 4. CLI

```text
xrun server --config <path>
xrun pair [--renew <device>]
xrun revoke <device>
xrun join <pair-url> [--name <name>]
xrun agent

xrun ls
xrun info <device>
xrun exec <device> [options] -- <program> [args...]
xrun <device> [options] -- <program> [args...]
xrun jobs [--device <device>] [--request-id <id>]
xrun job <job-id>
xrun logs <job-id> [--follow] [--after <seq>]
xrun kill <job-id>
```

`xrun exec <device>` 是稳定的完整语法，`xrun <device>` 是便捷简写。设备名采用 `[a-z][a-z0-9-]{0,31}`，拒绝当前子命令及 `help / version / config / login / logout / doctor / update / push / pull` 等保留名。未来若新增命令与旧名称冲突，旧设备仍可通过完整语法访问，不自动改名。新注册必须指定 `--name`，恢复身份使用原名称。

所有设备参数均可使用设备名或设备 ID；提交任务时解析并固定目标 ID，后续同名设备替换不能改变任务归属。

| 执行选项 | 语义 |
| --- | --- |
| `-C <cwd>` | 远端绝对工作目录；省略时使用 Agent 配置的默认目录 |
| `--env KEY=VALUE` | 覆盖远端进程环境变量，可重复指定 |
| `--stdin` | 提交前读本地 stdin 至 EOF，最多 1 MiB 原始字节；随请求发送到远端进程 |
| `--timeout <seconds>` | 从进程启动开始计算；默认 1800 秒，`0` 表示不设时限 |
| `--detach` | Server 接受任务后返回 Job ID，不等待执行结果 |
| `--request-id <id>` | 指定提交去重标识；省略时由 CLI 生成 |

`--detach` 返回成功只表示任务已受理，不表示进程已经启动或执行成功。

### 4.1 输出契约

- `ls / info / jobs / job` 支持全局 `--json`，输出单个 JSON 值；任务列表包含明确的字段名，不依赖表格位置。
- 前台执行默认将远端 stdout、stderr 分别写入本地对应流。xrun 的状态提示只写入 stderr，并带 `[xrun]` 前缀。
- CLI 在首次发送前输出 `request_id`，受理后输出 `job_id`；响应丢失时可按 `request_id` 查找。
- `xrun --json <device> ...` 使用 NDJSON 输出受理、状态、日志和结果事件；远端 stderr 也作为事件输出，不混入自然语言提示。
- `xrun --json logs ...` 输出带序号和流类型的 NDJSON；默认模式恢复原始字节并写入对应流。

前台命令正常退出且输出完整时，远端退出码在 `0..255` 范围内直接透传；超出范围时 CLI 返回 `1`，完整数值保留在结构化结果中。参数错误返回 `2`，执行层错误、超时、信号终止或无法确认结果返回 `125`，本地 Ctrl+C 返回 `130`。日志截断或补读失败也返回 `125`，同时保留已经确认的远端执行结果。

远端程序也可能返回这些数值，因此机器调用方应同时读取结果的 `origin`、`state` 和 `error.code`，不能仅凭退出码判断错误来源。`job / jobs` 查询成功只表示查询完成，其 CLI 退出码不代表被查询任务成功。

## 5. 命令执行语义

### 5.1 参数与 Shell

默认协议传递 `program` 和 `args[]`，Agent 直接创建进程，不自动拼接 Shell 字符串。可执行文件解析由 xrun 实现，解析后始终向系统传绝对路径：

- 绝对路径直接使用；包含目录分隔符的相对路径按远端 cwd 解析。
- 裸程序名只按最终环境的 PATH 顺序查找；空 PATH 项忽略，相对项按远端 cwd 解析，不隐式加入 Agent 所在目录或系统搜索目录。
- Windows 无扩展名时按最终 PATHEXT 顺序查找，未配置时使用 `.COM;.EXE;.BAT;.CMD`。找到批处理或脚本时返回 `SHELL_REQUIRED`，不能暗中通过 Shell 执行或继续搜索另一个同名程序。
- Unix 检查可执行权限；Windows 按目标程序约定编码命令行，并在创建前检查平台长度限制。操作系统限制仍可能导致启动失败。

需要管道、重定向或 Shell 内建命令时，显式执行目标设备上的 Shell。第一版不增加独立的 Shell 模式和默认 Shell 推断。

```bash
xrun mac1 -C '/Users/dev/Projects/valle' -- /bin/zsh -lc 'git diff --stat && cargo test'
xrun win1 -C 'D:\Projects\valle' -- powershell.exe -NoProfile -NonInteractive -Command 'Get-Content Cargo.toml'
```

调用方 Shell 会先处理引号、变量和转义。远端路径应加引号；cwd 不展开 `~` 或环境变量。上面的例子按 Bash/Zsh 调用端编写，其他调用端遵循各自的引用规则。

Windows 的 `.bat / .cmd` 和 `cmd.exe` 使用特殊参数解析规则，不能承诺与普通可执行文件相同的 argv 行为。第一版直接执行模式拒绝批处理文件；需要运行时显式调用 `cmd.exe /C`，由调用方提供目标 Shell 的引用。[Rust 进程文档](https://doc.rust-lang.org/std/process/struct.Command.html#method.arg)

### 5.2 工作目录与环境

Agent 启动时固定默认工作目录，默认值为运行用户的主目录。`info` 返回执行用户、主目录、默认 cwd 和配置的 PATH。目录不存在或不可访问时返回 `INVALID_CWD`，不得退回其他目录。

进程环境由 Agent 启动环境、Agent 配置中的环境覆盖项、请求中的 `env` 依次合并。直接执行不会加载交互 Shell 配置，不继承调用方机器的环境。xrun 内部凭证不通过环境变量注入子进程。

后台启动 Agent 时必须配置所需工具链路径。能力列表第一版仅作为诊断信息，不承诺某个工具或项目一定可构建。程序找不到时返回 `PROGRAM_NOT_FOUND`。

### 5.3 有界 stdin

默认 stdin 为 EOF。指定 `--stdin` 时，CLI 先将输入读到 EOF，最多保留 1 MiB 原始字节；超限或读取失败时拒绝提交，不启动远端进程。TTY 输入被拒绝，要求管道或重定向，避免意外等待交互。输入按字节传输，允许 NUL 和非 UTF-8 内容。

```bash
xrun exec win1 -C 'D:\Projects\valle' --stdin -- git apply - < change.patch
xrun exec mac1 -C '/Users/dev/Projects/valle' --stdin -- python3 - < edit.py
```

输入编码为 Base64 随执行请求一次性传送。Agent 验证完整请求后启动进程，并发写入 stdin、排空 stdout/stderr，写完后关闭输入管道；网络断开不影响已经接收的输入。`--detach --stdin` 同样必须先收完本地输入。

进程主动提前关闭 stdin 时，以进程结果为准；其他写入错误触发取消并报告 `STDIN_IO_ERROR`。Server 和 Agent 不将输入原文写入任务数据库或审计，转交完成后释放内存副本。不支持边运行边追加输入、终端交互或断点续传；这是一项执行输入能力，不提供文件路径映射与同步协议。

### 5.4 进程管理

- 每个 Job 管理一个主进程及其受管理的子进程。Unix 使用独立进程组；Windows 使用独立 Job Object，禁止普通子进程脱离。
- 主进程结束后清理同组残留子进程，再关闭输出并确认结果。自行脱离进程组、后台守护化和提权后的进程不承诺可管理。
- Unix 取消先向进程组发送 SIGTERM，5 秒后仍未退出则发送 SIGKILL。Windows 第一版直接调用 `TerminateJobObject`，没有优雅退出宽限期，可能留下应用锁文件或未完成写入；xrun 不自动删除这些文件。
- 每设备默认最多同时运行 4 个 Job，可配置。Agent 的 `starting / running` 和尚未确认停止的任务各占一个名额，恢复中的 `unknown` 也占用；确定终态释放名额。容量不足返回 `DEVICE_BUSY` 及占位 Job ID，不建立等待队列。

Windows 平台层使用 Win32 `CreateProcessW` 和 `PROC_THREAD_ATTRIBUTE_JOB_LIST`，在进程创建时完成 Job 关联；失败时不得退回未受管理的创建路径。要求 Windows 10 / Server 2016 或以上。Job Object 的唯一名称先随启动意图落盘，再创建进程；设置 `KILL_ON_JOB_CLOSE`，句柄仅由 Agent 持有、不可继承。Agent 崩溃时系统负责终止关联进程，但不能恢复丢失的退出结果。[进程属性](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-updateprocthreadattribute)、[Job Object](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)

这需要独立封装进程创建、参数编码、管道和句柄回收，不能仅靠稳定版 `std / tokio::process::Command` 的公共接口完成；Rust 对应进程属性接口目前仍为实验性接口。[Rust 文档](https://doc.rust-lang.org/std/os/windows/process/trait.CommandExt.html#tymethod.spawn_with_attributes)

### 5.5 超时与睡眠

超时按包含系统睡眠的实际经过时间计算，不受修改系统日期影响，不直接依赖跨平台 `Instant` 的睡眠行为。

| 平台 | 计时来源 |
| --- | --- |
| Linux | `CLOCK_BOOTTIME`，包含 suspend 时间。[文档](https://man7.org/linux/man-pages/man2/clock_gettime.2.html) |
| macOS | `mach_continuous_time`，包含睡眠时间。[源码](https://github.com/apple-oss-distributions/xnu/blob/main/osfmk/mach/mach_time.h) |
| Windows | `GetTickCount64`，包含睡眠与休眠时间，毫秒精度足以用于任务超时。[文档](https://learn.microsoft.com/en-us/windows/win32/sysinfo/interrupt-time) |

Agent 以最多 1 秒的检查间隔并结合系统恢复通知重检截止时间。睡眠期间不能执行终止操作，也不主动唤醒设备；恢复后发现超时即执行终止流程。网络断开不影响存活 Agent 计时，Agent 崩溃后的任务按第 6 节核对。

## 6. Job 生命周期与故障语义

### 6.1 统一任务模型

每次执行都创建 Job，前台和后台仅在 CLI 是否等待上有区别。

1. CLI 生成 `request_id` 并提交执行参数。
2. Server 验证调用方、目标白名单、在线状态和参数，持久化任务索引及请求摘要后返回 `job_id`；env 和 stdin 原文仅在内存中等待转交。
3. Server 先持久化 `dispatch_started=true`，再向目标 Agent 下发一次请求；标记不能因发送失败而回退。Agent 再次检查白名单，持久化启动意图后创建进程，记录进程身份并上报状态和输出。
4. Agent 完成进程清理和输出收尾后持久化终态，再通知 Server 和订阅方。

| 状态 | 含义 |
| --- | --- |
| `accepted` | Server 已受理，尚未确认 Agent 启动 |
| `starting` | Agent 已持久化启动意图，尚未确认进程创建成功 |
| `running` | 主进程已启动 |
| `exited` | 进程已结束，包含退出码或终止信号；非零退出码仍属于此状态 |
| `failed` | 已确认未启动，例如程序不存在、目录错误或容量不足 |
| `canceled` | 取消已完成，已确认受管理进程停止或尚未启动 |
| `timed_out` | 超时处理已完成，已确认受管理进程停止 |
| `lost` | 已确认不再存在受管理进程，但是否执行过及最终结果无法完整恢复；不是成功 |
| `unknown` | 暂时无法确认实际状态，可能仍在执行 |

`unknown` 是观测状态，可在取得 Agent 证据后恢复为实际状态，不表示失败或结束。Server 同时保留最后确认的状态及确认时间。Agent 的确定终态不可被迟到的运行事件覆盖。

### 6.2 去重与重试

Server 以 `(source_device_id, request_id)` 去重，Agent 以 `job_id` 去重。两端只保存规范化请求的 SHA-256 摘要用于比较：固定字段顺序、补全协议默认值、排序 env 键，包含目标 ID、program、args、请求 cwd、env、timeout，以及解码后 stdin 的长度和摘要。不同 env 值或输入字节必须产生不同摘要。

相同标识与相同摘要返回同一 Job；摘要不同返回 `REQUEST_CONFLICT`。通过身份和当前白名单检查后先查询去重记录，命中时不重新解析目标别名或检查在线状态。任务数据库保留必要执行元数据和摘要，不保留 env 值与 stdin 原文；任务详情接口也不返回它们。

断线或重启后只核对已有 Job，不自动重新下发执行请求。对已记录启动意图但结果不明的任务，不重新创建进程。Server 在受理与下发之间崩溃，内存中的请求原文允许丢失；通过下一节的核对确认未执行，不能为补偿丢失而重新运行。

第一版不承诺任意命令的 exactly-once 执行。调用方要再次执行时，必须明确创建新请求；xrun 不把网络重试变成命令重跑。去重记录不随日志清理而删除。

### 6.3 未下发任务的核对

Agent 初始化数据库时生成持久化 `store_id`，Server 在受理时记录目标 `store_id`。每次 Agent 连接使用新的 `session_id`；Agent 串行处理会话替换和任务操作，旧会话失效后不得再启动其中的命令。

恢复核对使用 `reconcile_job`，其处理与 `exec / cancel` 串行：

1. 已有记录：返回实际状态或按第 6.5 节核对；不能因结果暂缺而重跑。
2. 同一 `store_id`、存储历史完整且查无记录：事务写入 `failed(NOT_DISPATCHED)` 墓碑，再返回确认。该 ID 的任何迟到 `exec` 都被拒绝。
3. Server 收到上述持久化确认后，将任务标记为 `failed(NOT_DISPATCHED)`；确认前仍为 `unknown`。若执行请求先被 Agent 受理，则返回已有记录，不能写未下发墓碑。

数据库缺失、损坏、已知回滚或 `store_id` 改变时，“查无记录”不能证明从未执行。Agent 拒绝自动重建空库继续服务；需管理员重新建立设备身份，旧任务保持 `unknown`，直到有其他停止证据。手工恢复历史数据库不在自动核对保证内。墓碑和去重记录不随日志过期而删除。

### 6.4 断线与重启

| 事件 | 行为 |
| --- | --- |
| 提交时目标离线 | 拒绝提交，不排队等待设备上线 |
| CLI 退出或连接中断 | 已受理 Job 继续运行；CLI 可按 ID 查询、补读日志 |
| Agent 与 Server 断线 | Agent 继续执行、计时并记录日志；Server 将未确认的任务标记为 `unknown` |
| 连接恢复 | 先隔离旧会话，再按 Job ID 核对；仅补状态、未下发墓碑和日志，不重放执行请求 |
| Server 重启 | 从数据库恢复任务索引，等待 Agent 核对；恢复前不宣称任务已停止 |
| Agent 正常退出 | 停止接收请求，取消受管理任务并持久化结果后退出 |
| Agent 崩溃或设备重启 | 保留已落盘终态；其余先标记 `unknown`，按进程证据转为 `lost`，不接管或自动重跑 |
| Agent 状态存储故障 | 报告 `STORAGE_ERROR`；无法落盘的结果不能作为已确认终态上报 |

Agent 每个身份只允许一个本地实例，使用本地锁防止重复启动。Server 仅保留该设备最新的 Agent 连接，忽略旧连接迟到的消息。

网络连接采用心跳及带抖动的指数退避重连。默认 15 秒心跳、45 秒无响应判定离线，重连间隔上限 30 秒。凭证无效或被撤销时停止重试并报告错误。

### 6.5 unknown 的退出条件

Agent 保存系统启动标识 `boot_id`、主进程 PID、进程启动时间以及进程组或 Job Object 标识。仅凭 PID 不足以判定进程身份；Linux pidfd 可用于运行期稳定引用，但它是进程持有的文件描述符，不能作为 Agent 重启后的持久句柄。[pidfd 文档](https://man7.org/linux/man-pages/man2/pidfd_open.2.html)

| 核对证据 | 处理 |
| --- | --- |
| 本地存在确定终态 | 恢复该结果 |
| `boot_id` 改变，确认系统已重新启动 | 旧受管理进程已不存在，未落盘结果转为 `lost` |
| 同一次系统启动，确认整个原进程组已消失，或 Windows Job 已无活动进程 / 已销毁 | 结果无法恢复则转为 `lost` |
| 仅主进程消失，子进程组仍存在或无法核对 | 保持 `unknown`，不能以父进程死亡推断任务结束 |
| 缺少足以核对整个任务的标识，例如 Unix 创建后尚未落盘进程组身份就崩溃，或查询权限不足 | 保持 `unknown`，等待可验证的清理证据或系统重启 |

Agent 启动时、任务查询时和后台周期性执行核对。`lost` 是不可重跑的终态，退出码为 `null`、错误码为 `RESULT_LOST`；其副作用不能假定已回滚。`unknown` 保守占用并发名额；确认停止后释放，不能靠删除任务记录释放。无法证明身份时不自动杀旧 PID，需在目标机器处理遗留进程。

### 6.6 取消

`kill` 是幂等的取消请求，不是立即成功的状态修改。Server 只有在事务中确认 `dispatch_started=false` 时才能直接取消，并阻止随后下发；其他情况必须由 Agent 确认。Agent 对同一 Job 的启动与取消串行处理，取消先到时记录标记，禁止迟到的执行请求启动进程。

前台 Ctrl+C 发出一次取消请求；仅在收到确认后显示“已取消”。连接不可用或等待确认超时，输出 Job ID 和“取消未确认”。离线时不排队保存取消请求，调用方须在重连后重试。

取消、超时和自然退出竞态中，已持久化的确定终态优先。任务已结束时，取消返回现有结果。终止失败或进程仍未确认停止时返回 `unknown`，不能提前报告 `canceled / timed_out`。

## 7. 输出与日志

stdout 和 stderr 按字节流处理，不假定 UTF-8，也不按行等待。每个输出块包含 `job_id`、单调递增的 `seq`、`stream` 和 Base64 编码的字节数据，单块原始数据不超过 32 KiB。

同一流内保持顺序；两条流之间只保证 Agent 观察到的合并顺序，不承诺还原进程内部写入顺序。

Agent 先写本地日志再转发。订阅者使用最后收到的 `seq` 补读，重复序号可丢弃。终态包含 `last_seq` 和 `output_complete`，表示 Agent 的捕获范围是否完整；调用方还必须确认自身收到所有所需序号，不能将“Agent 捕获完整”等同于“本次下载完整”。

普通日志查询读取请求受理时的日志快照后结束；`--follow` 补读后继续订阅，收到终态并完成输出收尾后结束。订阅流包含状态和结果事件，避免依赖连接关闭推断执行完成。默认前台 CLI 在连接丢失后最多重连 30 秒；仍不可达则返回 `125` 和 Job ID，远端任务继续运行。

| 限制 | 默认行为 |
| --- | --- |
| 单 Job 日志上限 | 64 MiB，超过后继续排空进程输出，但停止保存并标记截断 |
| Agent 日志总量 | 1 GiB，优先删除已结束任务的旧日志；仍不足时截断当前日志 |
| 已结束任务日志保留 | 7 天；任务元数据及去重记录继续保留 |
| Server 输出缓存 | 每 Job 最后 64 KiB 已收到的完整输出块，总上限 64 MiB、保留 7 天；超过总量优先淘汰旧任务 |
| 订阅方消费过慢 | 断开该订阅并允许补读，不阻塞进程、控制消息或其他 Job |
| 目标设备离线 | 返回已持久化结果和可用缓存；没有缓存时返回 `LOG_UNAVAILABLE`，不伪造空日志 |

Server 对缓存报告 `log_source=server_cache`、首尾序号和缺口。任务终态未上报时只能返回最后确认的状态；缓存中出现“测试通过”不能替代终态。缓存写入可异步批量完成，崩溃后只报告实际落盘范围。

设备在线时优先从 Agent 补读。离线缓存只能满足部分范围时，CLI 展示可用内容、明确缺失范围并返回 `125`；`job` 查询仍可独立返回已确认的执行结果。缓存读取受目标最后确认的 `allow_from` 约束。

日志限额和保留时间可配置。磁盘写入失败、日志截断或日志已清理必须显式报告；不能返回空日志冒充完整结果。初始任务记录无法落盘时不得启动进程。

## 8. 接口与协议

使用由 xrun 自行提供的 HTTPS 处理查询和提交，WSS 处理 Agent 长连接及 CLI 日志订阅。消息使用 JSON，stdin 和输出字节使用 Base64。

第一版客户端与服务端统一升级，不设计多版本协议兼容层。HTTP 请求及 WSS 握手携带 `X-Xrun-Version`，Agent `hello` 同时报告 `agent_version`；与 Server 发布版本不一致时，在提交业务请求前返回 `VERSION_MISMATCH` 及双方版本。复用程序发布版本，不另外引入协议版本序列。

| 接口 | 作用 |
| --- | --- |
| `POST /pair` | 消费配对 Token，注册设备并签发证书 |
| `POST /devices/self/renew` | 已认证设备为同一身份续期证书 |
| `GET /devices`、`GET /devices/{id}` | 设备列表与详情 |
| `POST /jobs` | 提交执行请求，返回 Job |
| `GET /jobs`、`GET /jobs/{id}` | 任务列表与详情；支持按目标、request_id 查询 |
| `POST /jobs/{id}/cancel` | 请求取消并返回确认状态 |
| `WSS /jobs/{id}/logs?after=<seq>&follow=<bool>` | 日志补读和订阅 |
| `WSS /agent` | Agent 注册在线状态、接收控制、上报状态与日志 |

Agent `hello` 包含设备信息、`store_id`、`boot_id` 和 `allow_from`；握手确认后双方绑定本次 `session_id`。任务消息包含 `exec`、`state`、`output`、`result`、`cancel`、`reconcile_job`、`read_logs` 和心跳；关联 ID 匹配请求响应，`job_id` 区分任务。下发消息携带已认证来源 ID、目标 `store_id` 和 `session_id`；Agent 拒绝不匹配的存储或会话。

包含 stdin 的执行请求和对应 Agent 消息上限 2 MiB，其中执行元数据最多 64 KiB、解码后的输入最多 1 MiB；其他消息上限 1 MiB。接收时限制字节数，超限立即拒绝；再验证解码长度。连接、在途请求和控制 / 输出队列均有并发与内存上限，输出积压不能阻塞取消与心跳。

列表查询采用分页。program、args 和 env 禁止 NUL，stdin 不受此限制；Windows 同一请求内大小写重复的 env 键被拒绝。未知字段、非法类型和负数超时返回 `INVALID_REQUEST`，不隐式修正后执行。

执行请求示例：

```json
{
  "request_id": "9bc4c702-5ccb-4d5a-9e80-d458f2fb2b65",
  "target_device_id": "dev_mac1",
  "program": "cargo",
  "args": ["test"],
  "cwd": "/Users/dev/Projects/valle",
  "env": {"RUST_BACKTRACE": "1"},
  "stdin_base64": null,
  "timeout_seconds": 1800
}
```

结果示例：

```json
{
  "type": "result",
  "job_id": "job_01",
  "target_device_id": "dev_mac1",
  "cwd": "/Users/dev/Projects/valle",
  "origin": "process",
  "state": "exited",
  "exit_code": 0,
  "signal": null,
  "duration_ms": 13234,
  "last_seq": 42,
  "output_complete": true,
  "error": null
}
```

`stdin_base64=null` 或空字节输入均表示 EOF，规范化摘要按同一输入处理。`origin` 区分 `process` 与 `xrun`。进程未启动、被信号终止或结果未知时，`exit_code` 为 `null`，不得伪造为 `0`。实际 cwd 在 Agent 解析默认值并验证后返回；解析完成前允许为 `null`。

错误使用统一的 `error.code / error.message`。基础错误码按类别定义：

| 类别 | 错误码 |
| --- | --- |
| 身份与连接 | `UNAUTHENTICATED`、`DEVICE_REVOKED`、`SOURCE_NOT_ALLOWED`、`VERSION_MISMATCH`、`DEVICE_OFFLINE` |
| 参数与启动 | `INVALID_REQUEST`、`INVALID_CWD`、`PROGRAM_NOT_FOUND`、`SHELL_REQUIRED`、`SPAWN_FAILED`、`DEVICE_BUSY` |
| 输入 | `STDIN_TOO_LARGE`、`STDIN_READ_ERROR`、`STDIN_IO_ERROR` |
| 任务与存储 | `REQUEST_CONFLICT`、`JOB_NOT_FOUND`、`JOB_UNKNOWN`、`NOT_DISPATCHED`、`RESULT_LOST`、`LOG_UNAVAILABLE`、`STORAGE_ERROR` |

程序自身的非零退出不是协议错误。`lost` 和 `unknown` 在前台等待中均返回执行层退出码 `125`，不自动重试。

设备详情至少包含 `device_id`、`name`、`os`、`arch`、`hostname`、`agent_version`、`execution_user`、`home_dir`、`default_cwd`、`path`、`online`、`last_seen`；Agent 未上报过的环境字段为 `null`。断线后保留最后报告值并标明时间，不能视为当前环境探测结果。

## 9. 存储、部署与实现

### 9.1 持久化

| 位置 | 内容 |
| --- | --- |
| Server SQLite | 设备及证书、白名单副本、撤销状态、Token 摘要、任务元数据、请求摘要、有限输出缓存及审计 |
| Server 私有文件 | 部署 CA、服务端 TLS 私钥、证书和配置 |
| 设备私有文件 | 设备私钥和证书、固定的 CA、Server 地址、Agent 配置 |
| Agent SQLite | store_id、启动意图、进程身份、状态、结果、请求摘要、取消及未下发墓碑 |
| Agent 日志文件 | 带序号的输出块 |

Unix 私有目录权限为 `0700`、私钥文件为 `0600`；Windows 使用仅当前账户可访问的 ACL。日志与数据库按同等敏感数据管理。

审计记录时间、来源设备、目标设备、请求 ID、Job ID、程序、参数、cwd、状态、退出结果和耗时。认证失败、配对、恢复、撤销及取消同样记录。请求原文不得进入 HTTP 访问日志、调试日志或 SQLite；env 值和 stdin 只为本次转交保留在内存，崩溃后不恢复它们。

程序可能将输入或环境变量打印到输出，也可能把敏感值放入参数；这些内容仍可能进入日志或审计。上述规则减少主动持久化，不承诺自动识别和清除程序泄露的秘密。

### 9.2 部署

Server 以非特权用户运行，直接终止 TLS。配置示例中的 IP 是文档占位地址，部署时替换为实际公网 IP：

```toml
listen = "0.0.0.0:7443"
public_url = "https://203.0.113.10:7443"
data_dir = "/var/lib/xrun"
```

管理员开放 TCP 7443，准备可由 Server 账户写入的数据目录，执行 `xrun server --config <path>`，随后用 `xrun pair` 生成链接。CA、证书和数据库自动初始化；已有身份目录缺失部分状态时拒绝静默重建。无需 Nginx、域名、公网 CA 或 HTTP 明文过渡接口。

CA 和设备身份须与数据目录一起备份。公网 IP 改变时，Server 在同一 CA 下签发含新 IP SAN 的叶证书；客户端显式更新地址，不能接受网络返回的任意新地址。第一版不依赖反向代理注入客户端身份。

Linux Server 的本地管理接口使用 Unix socket，限制为 Server 运行账户访问。`pair / pair --renew / revoke` 通过该接口执行，不直接修改数据库。

Agent 默认以前台常驻进程运行。需要自动启动时，由用户配置 macOS LaunchAgent、Windows 用户登录任务或 Linux 用户级 systemd 服务；第一版不提供跨平台服务安装器。运行账户与环境必须与 `info` 展示一致。

Agent 正常停止会取消正在运行的任务，因此升级或修改白名单前应先停止新提交并等待任务结束。第一版不支持携带运行中任务热升级；Server 和全部 CLI / Agent 升级到同一发布版本后再恢复提交。

### 9.3 实现边界

使用 Rust、Tokio、rustls、JSON 和 SQLite，发布单个 `xrun` 二进制，内部按职责划分模块：

```text
cli       参数、展示和调用客户端
server    身份、设备连接、路由、索引和审计
agent     任务执行、进程管理和本地日志
protocol  请求、事件、状态和错误类型
```

先使用一个 Cargo package，不预拆成多个独立服务或公共库。平台层包含 Windows 原生进程创建与句柄管理、三端可执行文件解析、进程身份核对、包含睡眠的计时和本地权限。实现顺序先验证这些平台能力，再接入网络任务流。

## 10. 验收标准

以下场景均需有真实运行证据；跨平台行为在对应操作系统上验收。

| 场景 | 通过条件 |
| --- | --- |
| 纯公网 IP 部署 | 无域名和预装证书即可启动、配对；错误 CA 指纹、IP SAN 不符或证书过期均拒绝，Token 不经明文发送 |
| 三端配对与发现 | 每个 Token 只注册一个身份；CLI 和 Agent 均认证；离线与 last_seen 正确 |
| 来源限制 | 未列入目标 allow_from 的设备不能执行、取消或读取缓存；配对和设备重命名不扩大权限 |
| 云端执行 Mac、Windows 命令 | 指定 cwd 和参数正确到达，包含空格、反斜杠、Unicode 的参数不失真 |
| 本地执行云端命令 | cloud1 单独运行 Agent，调用不依赖 Server 进程隐式执行本地命令 |
| Git 开发流程 | 使用方准备分支，通过 xrun 执行 Git、查看或修改文件、运行构建；xrun 不自动同步文件 |
| stdin 修改文件 | 三端通过 stdin 应用补丁或运行编辑脚本；输入含二进制、超出平台命令行长度仍可传输；超过 1 MiB 不创建 Job |
| 执行环境 | 默认目录、指定目录、PATH 和 env 覆盖符合契约；目录和程序错误可区分 |
| 流式输出与退出 | stdout、stderr 运行中可见；非 UTF-8 输出保持字节；非零退出被准确返回 |
| 后台任务 | 受理后可查询 Job；CLI 退出不终止任务；日志可按序号补读 |
| 网络故障 | 分别在受理、启动、输出、结束阶段断线；重连后不重复执行，未知状态不冒充失败或成功 |
| 未下发核对 | 受理后、下发前崩溃可确认 NOT_DISPATCHED；墓碑拒绝迟到 exec；数据库缺失不误判 |
| Server 与 Agent 重启 | Agent 崩溃不重跑；父进程退出而子进程仍活时保持 unknown，确认全组停止后转 lost，名额按规则释放 |
| 取消与超时 | 在三端终止受管理的子进程；重复取消幂等，未确认取消明确报告 |
| Windows 创建与清理 | 创建即关联 Job；模拟 Agent 崩溃后关联进程被清理；结果丢失不冒充退出成功 |
| 设备睡眠 | 睡眠计入超时，恢复后检查截止时间；离线时可查询已确认结果和带明确范围的输出缓存 |
| 身份撤销 | 新请求、现有连接及续期均被拒绝；不宣称已经停止旧进程 |
| 凭证恢复与版本 | 同一公钥的过期身份可由管理员恢复，已撤销身份不可恢复；版本不匹配在提交前明确失败 |
| 输入与去重存储 | 更改 env 或 stdin 后复用 request_id 被拒绝；使用不回显输入的程序时，数据库、WAL、审计中无 env 值或 stdin 原文 |
| 容量与存储故障 | 并发满时不排队；慢订阅不阻塞任务；日志截断和存储失败可见，内存有界 |
| 机器接口 | JSON 字段稳定；stdout 不混入提示；结果能够区分程序失败与执行层错误 |

文件传输、自动同步、MCP、P2P、托管服务和细粒度权限须另行设计，不作为上述验收的前置条件。
