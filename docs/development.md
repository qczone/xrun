# 开发与发布

本文面向参与开发和发布 xrun 的人：构建、测试、发布流程，以及架构、协议和兼容规则。使用说明见 [使用手册](usage.md)。

## 仓库结构

| 路径 | 内容 |
| --- | --- |
| `src/` | 核心 Rust package：CLI、daemon 和 Linux 中转，三者是同一个二进制 |
| `desktop/` | 桌面 App：`src/` 为 React 前端，`src-tauri/` 为 Tauri 2 crate，`scripts/` 为构建、打包、发布和质量检查脚本 |
| `cloudflare/` | Cloudflare 中转：Worker + Durable Object（TypeScript）、部署脚本和 workerd 测试 |
| `scripts/` | 安装脚本 `install.sh`、`install.ps1` 和性能测量脚本 |
| `tests/` | Rust 集成测试、安装脚本测试和测试夹具 |
| `docs/` | 本文、使用手册和性能测量结果 `benchmarks/` |

核心使用 Rust；桌面端使用 Tauri 2、React、TypeScript 和 Vite；Bun 管理前端和 Cloudflare 子项目的依赖、脚本与测试。发布包不包含 Bun，用户运行时不需要 Rust、Node.js 或 Bun。

## 构建

安装 rustup 即可。[rust-toolchain.toml](../rust-toolchain.toml) 固定 Rust 版本及 rustfmt、clippy 组件，本地构建、CI 和打包使用同一工具链。

```bash
cargo build --locked --release
```

产物为 `target/release/xrun`（Windows 为 `xrun.exe`）。需要内置 Cloudflare 一键部署时，先打包 Worker 再构建 CLI；Bun 版本以 `cloudflare/package.json` 的 `packageManager` 为准：

```bash
bun install --cwd cloudflare --frozen-lockfile
bun run --cwd cloudflare package
XRUN_CLOUDFLARE_BUNDLE_DIR="$PWD/cloudflare/.wrangler/package" cargo build --locked --release
```

构建脚本校验 Worker 模块、版本、协议范围和 SHA-256 清单后，把它嵌入二进制。公共 OAuth Client ID、scopes 和回调地址来自 [cloudflare/oauth.json](../cloudflare/oauth.json)，构建时校验并编入程序；修改后必须重新构建，已安装的程序不会读取项目中的配置。

## 桌面端

需要 Rust、Bun，以及 macOS 的 Xcode 命令行工具，或 Windows 的 MSVC 与 WebView2。

```bash
bun install --cwd desktop --frozen-lockfile
bun run --cwd desktop dev          # 开发模式，前端热更新
bun run --cwd desktop check        # 格式、类型检查和界面测试
bun run --cwd desktop build        # 打包本机安装包
bun run --cwd desktop build --debug
bun run --cwd desktop build:ui     # 只构建前端
```

| 平台 | 产物 |
| --- | --- |
| macOS | `target/release/bundle/macos/xrun.app.zip`，同时保留 `xrun.app` |
| Windows | `target/release/bundle/nsis/` 下的当前用户安装 EXE |

- 入口 `desktop/scripts/desktop.ts` 先编译配套的 daemon（helper），再调用 Tauri；App 与 helper 的版本必须一致。设置了 `CARGO_TARGET_DIR` 时，产物和缓存放在该目录。
- `--target` 在同一系统内选择架构，例如在 Apple Silicon 上先执行 `rustup target add x86_64-apple-darwin`，再执行 `bun run --cwd desktop build --target x86_64-apple-darwin`，产物位于 `target/<target>/release/bundle/`。可用的 target 为 `x86_64-apple-darwin`、`aarch64-apple-darwin`、`x86_64-pc-windows-msvc` 和 `aarch64-pc-windows-msvc`。不生成 universal 包，也不支持跨操作系统构建；Windows 交叉编译需要对应架构的 MSVC 工具和 Windows SDK。
- macOS 本地包默认使用 ad-hoc 签名，可用 `APPLE_SIGNING_IDENTITY` 指定签名身份。正式包在 Package 工作流中用 Developer ID 签名、公证并附加票据后再打包 ZIP。Windows 安装包目前未签名。
- 开发模式可以在界面中启动和停止 daemon，复用本机身份和配置，日志写入 `~/.xrun/daemon-dev.log`；退出界面后 daemon 继续运行，也不设置登录启动。登录启动需要打包后的 App。
- `tauri.macos.conf.json` 指定 App，`tauri.windows.conf.json` 指定 NSIS。bundle identifier 为 `sh.xrun`，macOS 后台服务标签为 `sh.xrun.daemon`。

**终端 CLI。** macOS App 启动时把 `~/.local/bin/xrun` 链接到包内 helper，并在 zsh（`.zprofile`、`.zshrc`，遵循 `ZDOTDIR`）和 Bash 的配置中添加幂等的 PATH 块；`--install-cli` 不打开界面执行同样的配置，供安装脚本调用。开发模式、挂载磁盘和 AppTranslocation 位置不修改终端配置。Linux 安装脚本以同样的方式配置 PATH。Windows 包中 `cli/xrun.exe` 是控制台 CLI，根目录的 `xrun.exe` 是无控制台的 helper；安装器通过 App 的 `--install-cli` 写入 `HKCU\Environment\Path`，并用 `.xrun-cli-path.json` 记录自己添加的项，卸载时只移除该项。配置失败时报告 `CLI_INSTALL_FAILED`，Windows 安装器退出码为 34。

**macOS 服务排查。** `/Applications/xrun.app/Contents/MacOS/xrun-desktop --start-service` 复用界面的启动流程并输出状态，不打开窗口；`launchctl print gui/$(id -u)/sh.xrun.daemon` 查看登记和进程状态。SMAppService 的 `enabled` 只说明登记和授权状态，不能证明 launchd 已保存当前 helper 的签名；更新程序后按 Apple 的要求先注销再重新登记。本地 ad-hoc 包的检查不能代替正式公证包的验收。

## Cloudflare 子项目

`cloudflare/` 中的 Worker 与 Rust 中转共用设备认证和端到端会话协议。

```bash
bun install --cwd cloudflare --frozen-lockfile
bun run --cwd cloudflare check          # TypeScript 检查
bun run --cwd cloudflare test           # workerd 中的测试
bun run --cwd cloudflare test:interop   # 本地 workerd 与真实 Rust CLI、daemon 互通
```

`test:interop` 先用 Cargo 构建 CLI 和测试，复制到临时目录后运行，覆盖创建网络、配对、可靠任务、65 MiB 文件往返、二进制流、TCP 半关闭、管理设备离线、空闲恢复和撤销，不需要 Cloudflare 凭据。

**源码部署**（开发者自用，需要已登录的 Wrangler）：

```bash
bun run --cwd cloudflare deploy --name xrun-relay
bun run --cwd cloudflare remove --name xrun-relay
```

多账号时设置 `CLOUDFLARE_ACCOUNT_ID`。私有状态保存在 `cloudflare/.deploy/`（不要提交），与用户 CLI 的部署记录分开；删除脚本只接受该目录中有记录的实例，删除后再部署请换名称。

**OAuth 客户端。** CLI 使用 xrun 自己注册的公开 OAuth 客户端，注册参数见 [cloudflare/oauth-client.json](../cloudflare/oauth-client.json)：Authorization Code + PKCE S256，token endpoint auth method 为 `none`，不分发 client secret。默认只申请 `account-settings.read` 和 `workers-scripts.admin`（首次创建和删除 Worker 需要 Workers Admin），`relay login --remember` 额外申请 `offline_access`。自动部署只使用 workers.dev，不申请路由写入权限。客户端登记了以下回调：

```text
http://127.0.0.1:8976/oauth/callback
http://127.0.0.1:8977/oauth/callback
http://127.0.0.1:8978/oauth/callback
```

登录时依次尝试这三个端口，全部被占用时返回 `INVALID_PORT`。授权请求中的 scope ID 必须与客户端登记的一致，`.edit`、`.write` 和 `.admin` 不能互换。遇到 `OAUTH_INVALID_SCOPE` 时，按错误中的 `error_description` 核对，可以用 `GET /client/v4/oauth/scopes` 对照显示名称与 ID，并同步更新两份 JSON。联调时可用 `XRUN_CLOUDFLARE_OAUTH_CLIENT_ID` 覆盖 Client ID；Fork 应注册自己的客户端并修改 `oauth.json`。

**部署细节。** 上传前检查账号的 workers.dev 子域名：只有子域名查询返回 400/404 + `10007` 或 403 + `10063`、且错误全部为该码时，才视为未初始化并尝试注册；普通的 401/403 仍会中止部署。Worker 与两个 SQLite DO 类 `XrunRelay`、`XrunData` 一起上传，首次部署用一个 v1 migration 同时声明两者，永久删除使用 removed 标记。部署写入 `XRUN_PROTOCOL_MIN` / `XRUN_PROTOCOL_MAX`，远端用随机的所有权标记识别本机部署。部署进度由 `src/deployment.rs` 统一产生：CLI 输出文字或 NDJSON，桌面通过带操作编号的 Tauri 事件显示。

**真实部署集成测试。** 把完整地址写入仓库之外、权限为 0600 的文件：

```bash
XRUN_TEST_CF_LINK_FILE=/绝对路径/私有地址文件 \
  cargo test --locked --test cloudflare -- --ignored --nocapture
```

测试用正式的 CLI 和 daemon 创建隔离网络，覆盖任务、65 MiB 文件、流式执行、转发、管理设备离线、空闲恢复和撤销。本地 workerd 不能代替真实环境的休眠验证：`cloudflare/scripts/hibernation.ts` 配合临时部署的 `cloudflare/tests/workerd/deployed-probe.ts` 验证休眠重建，结束后清理临时 Worker。

## 测试与 CI

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
# Linux X11 截图（需要 Xvfb、libX11、libXrandr）
xvfb-run -a -s '-screen 0 1024x768x24' cargo test --locked --test screenshot -- --ignored
bun desktop/scripts/check-quality.ts
```

- 单元测试放在源码末尾的 `#[cfg(test)] mod tests` 中，集成测试在 `tests/`。桌面原生 UI 测试需要在主线程运行，入口为 `desktop/src-tauri/tests/native_ui.rs`（`harness = false`）。
- `tests/cli_docs.rs` 和 `tests/smoke.rs` 检查内置手册与 `docs/usage.md` 一致，且每个 `xrun doc` 章节存在并唯一。修改 `docs/usage.md` 的二级标题时，同步更新 `src/cli/docs.rs` 的章节表和该测试。
- 改变签名记录的结构或编码时，必须同步更新 `tests/fixtures/signatures.json`，不能因为字段顺序变化而无意改变签名字节。

| 工作流 | 触发 | 内容 |
| --- | --- | --- |
| [Test Linux](../.github/workflows/test-linux.yml) | push、PR | x86_64 与 arm64：fmt、clippy、全部测试（1 MiB 主线程栈）、X11 截图、真实 systemd 服务、workerd 测试与互通、安装脚本 |
| [Test macOS](../.github/workflows/test-macos.yml) | push、PR | ARM：核心测试、桌面检查、调试包与 helper smoke、安装脚本与回滚 |
| [Test Windows](../.github/workflows/test-windows.yml) | push、PR | x86_64 与 arm64：核心测试、桌面检查、NSIS 安装、静默升级与卸载 |
| [Readability](../.github/workflows/quality.yml) | push、PR | 源码行长、文件规模、Cloudflare 判别联合与非空断言，并测试检查器自身 |
| [Package](../.github/workflows/package.yml) | 手动 | 构建六组发布产物，见“发布” |
| [Release](../.github/workflows/release.yml) | 手动 | 用成功的 Package 产物创建 Release |
| [Measure performance](../.github/workflows/performance.yml) | 手动 | 与选定基线对照的性能测量 |

每次提交是否通过，以对应的 Actions 结果为准；失败时工作流会上传测试子进程的日志。

## 发布

**1. 同步版本。** 以下位置的版本必须一致，`release-assets.ts` 会检查：`Cargo.toml`、`desktop/src-tauri/Cargo.toml`、`Cargo.lock` 中的两个本地包、`desktop/package.json` 和 `desktop/src-tauri/tauri.conf.json`。

**2. 打包。** 在 main 上手动运行 Package：

```bash
gh workflow run package.yml --ref main
```

Package 为 Linux、macOS、Windows 各构建 x86_64 与 arm64 共六组产物，嵌入 Cloudflare Worker，并在安装后的 helper 或 CLI 上运行 `relay deploy --provider cloudflare --dry-run --json`，确认版本、摘要和内置的 OAuth 配置（检查时移除运行时的 Client ID 覆盖）。Linux 和 Windows 在对应架构的原生 runner 上构建和检查；macOS 两组都在 ARM runner 上构建，Intel 版通过 Rosetta 运行 smoke 和安装检查，不能代替 Intel 实机验收。macOS 签名和公证需要工作流中列出的凭据。Package 只读仓库，只产生 Actions 附件。

| 平台标识 | 下载文件 | 清单 |
| --- | --- | --- |
| `linux-x86_64` / `linux-arm64` | `xrun-linux-<架构>.tar.gz`，只含 `xrun` | `xrun-linux-<架构>.json` |
| `darwin-x86_64` / `darwin-arm64` | `xrun-app-darwin-<架构>.zip`，只含 `xrun.app` | `xrun-darwin-<架构>.json` |
| `windows-x86_64` / `windows-arm64` | `xrun-app-windows-<架构>.exe` | `xrun-windows-<架构>.json` |

产物中的架构统一写作 `x86_64` 和 `arm64`（Rust target 使用 `aarch64`）。清单由 [package-manifest.ts](../desktop/scripts/package-manifest.ts) 生成，记录版本、组件和各文件的 SHA-256。本地准备相同的清单：`bun desktop/scripts/package-manifest.ts --platform darwin-x86_64 --directory dist`。

**3. 发布。** 用成功的 Package 运行 ID 手动运行 Release：

```bash
gh workflow run release.yml --ref main -f package_run_id=<运行ID>
```

Release 确认该运行来自本仓库的 main 且全部成功，检出其提交并下载六组产物，不重新构建。[release-assets.ts](../desktop/scripts/release-assets.ts) 再次检查清单、SHA-256、安装脚本和源码版本，创建指向该提交的 `v<版本>` Release，上传全部文件后公开。版本含预发布标识时标记为 prerelease。已有同名标签时报错，不覆盖；上传失败遗留的草稿需要先处理。Release 包含六个下载文件、六份清单，以及共享的 `install.sh`、`install.ps1`、`LICENSE` 和 `SHA256SUMS`。只有 Release 的发布任务有仓库写入权限。

**内置手册。** `docs/usage.md` 编译进 CLI，`xrun doc` 与仓库使用同一份文件，修改后需要重新构建。`usage.md#安装` 和 `usage.md#中转部署` 被 Release 说明和官网引用，修改这两个标题时同步更新引用处。

**发布前检查：**

- 对应提交的三平台 CI 全部通过。
- 用正式签名、公证的 App 实测创建网络、加入、邀请和撤销，以及 Cloudflare 浏览器授权和真实账号部署。
- 运行真实 Cloudflare 集成测试。
- 检查 Mac 重新登录、睡眠唤醒和云主机重启后的服务与连接。目前没有 Windows 实机环境，Windows 的登录服务、命名管道、截图和锁屏只有 CI 自动化覆盖。
- 涉及兼容性的改动符合下文的版本规则。

## 架构

### 组成

```text
开发者 / AI → 本机 CLI → 本机 daemon（复用会话）
                 │ 主动连接
                 ▼
     中转（Rust 或 Cloudflare）：认证成员、接通连接、转发密文
                 ▲
                 │ 主动连接
             目标 daemon

本机 CLI / daemon ←── 中转隧道内的设备间双向 TLS ──→ 目标 daemon
```

| 组件 | 职责与数据 |
| --- | --- |
| 中转 | 认证成员证书，接通连接，转发密文；保存网络归属、根签名的成员清单与有效期、流量统计；不保存任务、日志或文件 |
| 管理设备 | 持有网络根私钥和权威成员数据库（`manager/`），签发成员证书、成员清单、有效期和邀请 |
| daemon | 用端到端证书确认来源，检查本机授权，执行任务、传输文件、截图和转发；保存 Job、日志和附件；通过私有 IPC 为本机 CLI 缓存会话 |
| CLI | 发起调用，保存本机提交记录（`submissions.sqlite`），取回文件 |
| 桌面 App | 通过核心库的公开边界管理本机，读取本机 Job；执行仍由独立的 daemon 完成 |

代码按职责划分：`membership` 负责签名清单与权威状态，`network` 负责创建、加入、配对、续证和端点同步，`relay` 是 Rust 中转，`secure` 建立端到端 TLS，`ipc`、`session`、`pool`、`multiplex` 处理本机通信与连接复用，`daemon`、`store` 管理可靠任务，`process` 管理原生进程，`transfer`、`screenshot`、`forwarding`、`streaming` 实现各项业务，`service`、`control`、`history` 提供本机服务与只读活动，`cli` 是命令入口。公开边界为 `xrun::client`（用户操作、设置和只读历史）、`xrun::runtime`（进程入口）以及 `protocol`、`error`；`xrun::testing` 只供集成测试使用。`deny(unreachable_pub)` 防止意外公开，新增的请求种类必须加入穷尽分发。

### 连接

1. **外层**：设备通过 HTTPS/WSS 连接中转。Rust 中转用部署链接中的 CA 指纹固定证书，Cloudflare 中转使用公共 WebPKI。中转为每个状态、控制和接通请求发出随机挑战，成员用私钥签名，绑定网络、设备 ID、接口路径和挑战；管理设备另用根私钥签名，中转据此标记管理设备。不带成员证书的连接只能接通管理设备，用于加入和过期续证。
2. **控制连接**：每个 daemon 保持一条控制连接并接收会话通知，只有持有设备私钥的连接才能占用该设备的路由。每 15 秒 ping，45 秒无响应判定离线；断线后按指数退避重连，最长间隔 30 秒。
3. **端到端**：会话接通后，两台设备在隧道内建立双向 TLS，验证网络根、成员证书和预期的设备 ID；先交换清单摘要，不一致时才交换完整的根签名清单。来源身份取自客户端证书，中转提供的路由 ID 只用于选路。
4. **复用**：daemon 为每个设备对保留一条通道，用 yamux 多路复用最多 32 个操作流（接收窗口 8 MiB，分片 16 KiB）。每个流在业务请求前重新检查授权、暂停代次、证书期限、成员有效期和数据库。空闲 5 分钟关闭，最多保留 3 条空闲通道。CLI 通过私有 IPC 借用通道：Unix 使用私有目录中的 socket 并核对用户，Windows 使用命名管道并核对用户 SID；本机没有 daemon 时直接建连。连接层从不自动重放已提交的请求。
5. **局域网直连**（可选）：只在所选私有地址上监听，候选地址只经已认证的端到端连接交换，安全检查与中转路径相同；失败时在业务请求前退回中转。

### 身份、成员清单与撤销

- 网络 ID 为 `net_<根公钥 SHA-256 指纹的 base32>`；设备 ID 为随机的 32 位十六进制，名称只是别名。
- 根签名的成员清单包含网络 ID、递增的版本、管理设备 ID、管理代次、成员（ID、名称、公钥指纹、撤销状态）、中转地址和中转 CA；权限不在清单中。设备在 `roster.db` 中保存见过的最高版本，旧版本不能覆盖新版本，同版本内容不同时返回 `ROSTER_CONFLICT`。一个网络最多 256 个成员（含已撤销），最多 256 份有效邀请。
- 撤销先在管理设备本地提交并签名，再发布到中转；中转和在线设备确认版本与哈希，管理设备报告未送达的设备。签名只能证明清单来自管理设备，不能证明它是全网最新。
- 成员有效期（lease）由根私钥签名，绑定清单版本、管理代次和哈希，默认 15 分钟，管理设备每三分之一周期续签一次。设备和中转拒绝在过期的清单上开始新业务（`ROSTER_EXPIRED`），以此限制中转扣住撤销的时间。
- 网络根约 20 年有效；成员证书最长 365 天，剩余不足 30 天时由 daemon 自动向管理设备续期。
- 接入码共八位，字符表为 `23456789ABCDEFGHJKLMNPQRSTUVWXYZ`：前两位是公开的会话编号，后六位（30 位随机秘密）只用于设备之间的 SPAKE2 验证。每个接入码最多验证 5 次，预算在管理数据库中持久扣减；中转只保存公开编号、邀请 ID 和过期时间。验证通过后，再通过固定指纹的 TLS 交付强随机邀请并签发身份。
- 管理身份备份使用 Argon2id（32 MiB、3 次迭代）派生密钥，以 AES-256-GCM 加密，单个备份最大 32 MiB。恢复和交接会推进管理代次并保留之后发生的撤销；过时的根密钥持有者会自动删除根私钥。

### 授权

每个 daemon 在 `daemon.toml` 中以设备 ID 保存授权。判断顺序：有效且未撤销的成员 → 未暂停 → 未被单独拒绝 → 在允许列表中，或开启了全体信任。管理设备只接受逐台授权，不接受全体信任。任务查询、日志和取消还要核对任务属于该来源。配置变更由文件锁串行化；daemon 监听文件变化（60 ms 合并，监听失败时每 2 秒轮询兜底），读取或验证失败时一律拒绝。暂停代次保证快速暂停再恢复也能让旧会话失效。

### Job 模型

`exec`、`stream_exec`、`push`、`pull`、`screenshot` 和每条 `forward` 连接都是目标设备上的一个 Job。表结构的唯一定义是 [src/store/schema.sql](../src/store/schema.sql)。

- 受理事务先按 `(source_device_id, request_id)` 去重：ID 和请求摘要都相同时返回原 Job，摘要不同时返回 `REQUEST_CONFLICT`；然后检查并发配额，持久化后才开始执行。并发已满时返回 `DEVICE_BUSY`，不写去重记录。
- 每个请求携带目标的 `db_id`，任务库重建后旧请求返回 `DB_RESET`。CLI 在发送前把提交意图写入 `submissions.sqlite`，保留 7 天。
- 退出码 75 表示请求可能已送达但结果未确认。CLI 在开始写网络之前就标记“可能已发送”，写入失败也不视为未发送。
- 最终状态持久化后不会被迟到的事件覆盖。daemon 崩溃重启后，未完成的 Job 标记为 `lost`（按 boot_id、PID 和进程启动时间判断能否安全清理遗留进程），不自动重跑。
- 输出按批持久化（满 32 KiB、首字节后 50 ms 或 EOF 时提交），事务成功后才通知订阅者；线上传输为二进制，CLI 的 JSON 和桌面 API 使用 Base64。
- 文件和日志按块封装，记录编码、原始长度和 SHA-256；自动模式只在至少节省 1/8 时使用 zstd。
- 文件续传：目标在 `job-transfers/` 保存检查点和暂存内容，客户端在 `client-transfers/` 保存不可变副本；每 1 MiB 先 fsync 内容，再落盘偏移。中断的传输标记为 `interrupted`，只有明确的 `resume` 才会继续；24 小时后清理，每个目标最多 32 个暂存。
- 附件是独立副本，元数据在 `job_attachments` 表中；环境变量的值、stdin、脚本、文件和图片原文不写入 Job 表。

### 中转实现

**Rust 中转**（`xrun relay`，仅 Linux）：配置在 `~/.xrun/config.toml`，传输 CA 和服务端证书在 `~/.xrun/server/`，网络归属在 `owner.json`，流量在 `traffic.sqlite`（WAL/NORMAL，每 10 秒批量写入）。在线连接和会话只保存在内存中。最多 512 条 TCP 连接，每个 IP 最多 16 条未认证连接；每个来源最多 16 个会话，每个目标 32 个，全局 4096 个。

**Cloudflare 中转**：每个部署有一个固定的 `XrunRelay` 对象，负责网络归属、控制连接、配对、全局额度和流量汇总；每条获准的数据通道使用一个以随机会话 ID 命名的 `XrunData` 对象，控制对象不转发密文。源和目标各持一张一次性票据，绑定网络、设备、控制代次和期限。每个网络最多 8 个来源会话（每个已认证来源最多 4 个，每个匿名 IP 最多 4 个，最后 1 个预留给管理设备）。每个方向最多 4 MiB 未确认的密文，接收端每 256 KiB 回复一次 ACK，单帧最大 64 KiB。WebSocket 附件保存协商版本、空闲状态和流量计数，休眠恢复后继续生效；流量每 10 秒、或在查询和关闭时汇总到 SQLite。

**流量统计**：两种中转共用 `TrafficQuery` / `TrafficReport` 契约，按网络和设备记录 UTC 每日收发量（`traffic_usage_daily`）。只统计已绑定会话的有效密文二进制帧，认证、ACK、缓存声明和心跳不计入。录入失败时继续转发，并标记 `complete: false`；存储无法读取时返回 `STORAGE_ERROR`。

### 版本与兼容

| 版本 | 用于 | 规则 |
| --- | --- | --- |
| 发布版本 | `xrun --version`、诊断、本机 IPC | 同一设备上的 CLI、App helper 与 daemon 必须一致 |
| 协议版本 | 外层 WSS（`X-Xrun-Protocol: min-max`）、端到端清单交换、配对和 Ready | 协商双方共同的最高版本；没有交集时返回 `VERSION_MISMATCH`，不发送业务、不消费邀请 |
| 签名格式 | 签名载荷 `xrun/sig-v1/<用途>\0` 加固定 JSON | 与发布版本无关，由 `tests/fixtures/signatures.json` 固定 |
| 数据库 schema | Job、submissions、成员库和流量库 | 只接受当前结构；未知或无版本的库返回 `DB_SCHEMA_MISMATCH`，不自动改动 |

正式发布前统一维护协议 1、签名格式 1 和 schema 1 的当前基线；内部重构直接更新该基线，不为旧 RC 添加迁移或混合版本支持。升级时统一更新所有设备和中转。同一编号和协议范围协商不构成 RC 之间的业务格式兼容保证，正式版的兼容范围和升级方式在正式发布时明确。

普通消息会忽略可以安全忽略的未知字段，但必需字段和未知消息类型仍会拒绝；签名记录、证书证明和本机安全配置严格拒绝未知字段。

### 主要限制

| 项目 | 限制 |
| --- | --- |
| 单条业务消息 | 1 MiB |
| 可靠执行的 stdin、脚本 | 各 1 MiB |
| 文件传输、流式输入、TCP 转发 | 无固定总量，按 64 KiB 分块 |
| 截图 | 编码后 64 MiB，最多 3200 万像素 |
| 单任务日志 / 日志总量 | 64 MiB / 1 GiB，已结束任务的日志保留 7 天 |
| 同时运行的命令 | 默认 4，可设 1–64 |
| 文件与截图并发 | 每台设备 8 个 |
| 设备对通道 | 每条 32 个流，最多 3 条空闲通道 |
| 成员 / 有效邀请 | 各 256 |
| 端口转发连接 | 每个转发进程 32 条，每个 daemon 32 条 |

## 性能测量

共享 CI 中与时延相关的功能测试使用 5 秒上限，避免把调度抖动当作回归。100 ms 输出可见、200 ms 授权失效是空闲机器上的验收预算，需要单独运行：

```bash
XRUN_LATENCY_OUTPUT=target/latency.json \
  cargo test --locked --test latency -- --ignored --nocapture --test-threads=1
```

[Measure performance](../.github/workflows/performance.yml) 与选定的基线对照，在三平台测量空闲 CPU、日志写入、同期查询和中转吞吐，结果作为报告上传，不设阈值。本地运行：

```bash
bun desktop/scripts/benchmark-core.ts <基线ref>
XRUN_BENCH_LOG_MODE=bursts bun desktop/scripts/benchmark-core.ts <基线ref>
bun scripts/benchmark-relay.ts <基线ref>               # Rust 中转
bun cloudflare/scripts/benchmark-relay.ts <基线ref>    # 本地 workerd
```

结果默认写入 `output/benchmarks/`。在 Linux 上设置 `XRUN_BENCH_TRACE=1` 可统计文件读取和数据库同步调用，跟踪期间的耗时不参与比较。`scripts/benchmark-cache.ts` 用三台目标和 10、30、60 秒的调用间隔测量会话缓存，需要设置 `XRUN_TEST_BINARY`、`XRUN_TEST_CF_LINK_FILE` 和 `XRUN_BENCH_CACHE_OUTPUT`。

已保存的结果见 [docs/benchmarks](benchmarks/)。2026-10-13 在 M2 Pro 上使用 Rust debug 构建：Rust 中转的 64 MiB 转发从 210 提升到 390 MiB/s，64 字节回显的 p50 从 0.241 ms 降到 0.134 ms；本地 workerd 的逐帧 SQLite 行修改从 8192 降到 0，吞吐约从 220 提升到 245 MiB/s，数据分片后为 236–243 MiB/s。这些都是本地串行基准，不代表跨地域时延、平台扩展能力或 Cloudflare 账单。
