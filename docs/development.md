# 开发与发布

项目介绍见 [README](../README.md)，日常使用见 [使用手册](usage.md)，架构、协议与验收边界见 [设计文档](design.md)。

## 构建

先安装 rustup。项目通过 [rust-toolchain.toml](../rust-toolchain.toml) 固定 Rust 版本及 rustfmt、clippy 组件，本地构建、三平台 CI 和打包使用同一工具链；在项目目录运行 Cargo 时由 rustup 自动选择。

```bash
cargo build --locked --release
```

将 `target/release/xrun`（Windows 为 `xrun.exe`）放到固定目录并加入 PATH，再安装服务。CLI、daemon 和 Linux Rust 中转使用同一个二进制；Cloudflare 中转单独部署。网络组件按支持的协议范围协商，本机 CLI / App helper 与 daemon 要求发布版本一致。发布要求见 [design.md 的版本说明](design.md#63-版本和限制)。

GitHub Actions 的 [Package](../.github/workflows/package.yml) 工作流可手动构建 Linux、macOS、Windows 的 x86_64 与 arm64，共六组产物。Linux 提供只含 `xrun` 的 CLI 压缩包，macOS 提供 App ZIP，Windows 提供当前用户 NSIS 安装 EXE；桌面包自带终端 CLI，不再单独分发 Mac／Windows CLI 包。macOS App 的签名、公证需要配置工作流列出的凭据。每组产物还包含版本和 SHA-256 清单，以及对应的自动安装脚本。Package 只构建和检查产物；发布由独立的 [Release](../.github/workflows/release.yml) 工作流手动触发。

Linux 与 Windows 的两种架构分别使用对应的原生 runner；macOS 两组均使用 ARM runner，Intel 版交叉编译并通过 Rosetta 执行 smoke 和安装检查，不代表 Intel 实机验收。构建缓存和产物按目标架构分开。

## 桌面开发与打包

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
| macOS | ZIP，里面只有完整 `xrun.app` | `target/release/bundle/macos/xrun.app.zip` |
| Windows | 当前用户的 NSIS 安装程序 | `target/release/bundle/nsis/` 下的安装 `.exe` |

macOS 构建同时保留 `target/release/bundle/macos/xrun.app`；签名检查通过后，用 `ditto` 将同一份 App 打包为 ZIP。正式工作流会先完成 App 公证及票据附加，再生成 ZIP。

同一系统内可用 `--target` 指定目标架构，App 与内嵌 helper 会使用同一个 Rust target。首次使用先安装目标标准库，例如在 Apple Silicon Mac 上构建 Intel 包：

```bash
rustup target add x86_64-apple-darwin
bun run --cwd desktop build --target x86_64-apple-darwin
```

| 系统 | x86_64 target | arm64 target |
| --- | --- | --- |
| macOS | `x86_64-apple-darwin` | `aarch64-apple-darwin` |
| Windows | `x86_64-pc-windows-msvc` | `aarch64-pc-windows-msvc` |

Windows 交叉编译还需安装对应架构的 MSVC 工具与 Windows SDK。显式指定 `--target` 后，产物位于 `target/<target>/release/bundle/`，加 `--debug` 则为 `target/<target>/debug/bundle/`。不指定 target 时保留上表的本机默认路径。每次生成单一架构的包，不生成 macOS universal 包，也不支持从另一个操作系统构建桌面安装包。

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

桌面入口为 `desktop/scripts/desktop.ts`，先编译配套 daemon，再调用 Tauri；打包时自动构建前端，两者完整版本必须一致。平台配置 `tauri.macos.conf.json` 和 `tauri.windows.conf.json` 分别指定 App 和 NSIS。默认构建目录是项目根目录的 `target/`；如果设置了 `CARGO_TARGET_DIR`，构建缓存和产物会使用指定目录。前端源代码在 `desktop/src/`，构建输出在 `desktop/dist/`。macOS 本地包默认对整个 App 和内嵌程序做 ad-hoc 签名，打包后校验签名；正式发布需要 Developer ID 签名和公证，可通过 `APPLE_SIGNING_IDENTITY` 指定签名身份。GitHub Actions 先完成 App 签名、公证和票据附加，再压缩为 ZIP；不再生成 DMG。Windows 安装包目前未签名。

macOS 安装版 App 在启动时配置当前用户的终端 CLI；`--install-cli` 可在不打开界面的情况下执行同一配置，由自动安装脚本调用。开发模式、挂载磁盘和 AppTranslocation 临时位置不修改终端配置。CLI 使用指向包内 helper 的 `~/.local/bin/xrun` 链接，zsh 的 `.zprofile` / `.zshrc`（遵循 `ZDOTDIR`）及 Bash 当前有效的登录配置 / `.bashrc` 添加幂等 PATH 块。配置文件原有内容、权限和符号链接保留，写入失败会在 App 中报告 `CLI_INSTALL_FAILED`，不妨碍打开主窗口。`--self-check` 不执行安装配置。

Linux 自动安装脚本配置同样的 zsh 和 Bash 用户配置文件；PATH 指向实际安装目录，支持空格、单引号、符号链接配置文件，保留原有内容和权限。重复安装不重复追加 PATH 块；不可写或不完整的配置报告 `CLI_INSTALL_FAILED`。

Windows 安装包在 `cli/xrun.exe` 中包含控制台版 CLI，根目录 `xrun.exe` 仍是无控制台 daemon helper。NSIS 安装后调用 App 的 `--install-cli` 写入 `HKCU\Environment\Path`，保留原有注册表类型、未展开的变量和长 PATH，并广播环境更新。安装器通过 `.xrun-cli-path.json` 记录自己添加的项，卸载准备成功后只移除该项；预先存在的用户 PATH 项不归安装器所有。配置失败返回安装器退出码 34 和 `CLI_INSTALL_FAILED`。Windows 安装回归同时检查 PowerShell / CMD 按名称运行 CLI、重复安装、卸载清理和配置失败。

## 验证

Rust 模块内部测试统一放在对应源码末尾的 `#[cfg(test)] mod tests` 中，平台限定条件按需保留。集成测试放在 `tests/`。桌面端原生 UI 测试需要在进程主线程运行，独立入口保留在 `desktop/src-tauri/tests/native_ui.rs`，使用 `harness = false`。

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
# Linux X11 截图测试（需安装 Xvfb、libX11、libXrandr）
xvfb-run -a -s '-screen 0 1024x768x24' cargo test --locked --test screenshot -- --ignored
```

[Test Linux](../.github/workflows/test-linux.yml) 和 [Test Windows](../.github/workflows/test-windows.yml) 在 x86_64 与 arm64 runner 上分别原生运行核心测试；[Test macOS](../.github/workflows/test-macos.yml) 在 ARM runner 上原生运行。核心测试覆盖配对、授权、执行、任务、文件传输、流式执行、端口转发、会话缓存和故障恢复。测试直接调用 Rust 中转库，`xrun relay` 部署命令仍仅支持 Linux。Linux 工作流另运行 Xvfb 截图测试和 Cloudflare workerd 测试；macOS、Windows 还检查桌面端、安装包内的 helper 和自动安装脚本，覆盖中文及空格路径、重复安装、损坏产物、版本不符，以及 macOS 回滚和 Windows 静默升级／卸载失败。macOS x86_64 的产物检查在 Package 工作流中通过 Rosetta 执行。每次提交是否通过，以对应的 Actions 结果为准。

Cloudflare 中转的本地检查无需云端凭证：

```bash
bun run --cwd cloudflare check
bun run --cwd cloudflare test
```

Ubuntu 26.04 x86_64 云主机与 macOS ARM64 本机已通过公网 TCP 8080 实机检查：双向执行与文件传输、任务去重/等待/取消、授权、繁忙重试、中转中断后的日志恢复，以及 systemd 和 LaunchAgent 的异常重启。另已通过真实 Cloudflare 中转验证独立 CLI 进程间的会话复用、64 MiB 文件往返、空闲恢复和撤销。云端无桌面时截图返回 `NO_DISPLAY` 和退出码 1。

系统服务、真实桌面截图权限/锁屏、设备睡眠和磁盘故障的验收进度见 [design.md](design.md)。

## 发布与内置文档

Package 工作流只生成 Actions 附件，使用 `gh workflow run package.yml --ref main` 触发。对外发布时，在 main 上单独手动运行 Release，填写已经成功完成的 Package 运行 ID；例如 `gh workflow run release.yml --ref main -f package_run_id=<运行ID>`。Release 校验该运行来自本仓库 main 上的 Package 工作流并已全部成功，再检出其对应提交、下载其六组产物，不重新构建。[release-assets.ts](../desktop/scripts/release-assets.ts) 再次检查六组清单、SHA-256、安装脚本和源码版本一致性，创建指向 Package 原始提交的 `v<完整版本>` Release，上传全部文件后公开。含预发布标识的版本标记为 prerelease。已有同名标签会报错，不覆盖已发布版本；上传失败遗留的草稿需处理后再发布。Package 只读仓库，只有独立 Release 的发布任务具有写入权限。

产物清单由 [package-manifest.ts](../desktop/scripts/package-manifest.ts) 使用现有 Bun 生成；用户运行安装程序不需要 Bun，见 [安装说明](usage.md#安装)。

产物中的架构统一命名为 `x86_64` 和 `arm64`（Rust target 使用 `aarch64`）：

| 平台标识 | 下载文件 | 清单中的组件 | 清单 |
| --- | --- | --- | --- |
| `linux-x86_64` / `linux-arm64` | `xrun-<平台标识>.tar.gz` | `cli` | `xrun-<平台标识>.json` |
| `darwin-x86_64` / `darwin-arm64` | `xrun-app-<平台标识>.zip` | `app` | `xrun-<平台标识>.json` |
| `windows-x86_64` / `windows-arm64` | `xrun-app-<平台标识>.exe` | `app` | `xrun-<平台标识>.json` |

本地需要准备与发布相同的清单时，先将相应产物放入目录，再运行例如 `bun desktop/scripts/package-manifest.ts --platform darwin-x86_64 --directory dist`；清单记录各文件的 SHA-256，并复制对应的自动安装脚本。

Release 分别提供六个下载文件、六份清单，以及共享的 `install.sh`、`install.ps1`、`LICENSE` 和 `SHA256SUMS`。Mac ZIP 内只有 `xrun.app`，Linux 压缩包内只有 `xrun`；README、文档、开发配置和安装脚本不混入压缩包。`docs/usage.md` 编译进 CLI，随程序发布，仓库和离线 `xrun doc` 使用同一份手册；更新后需要重新构建。文档测试验证无身份、无网络的帮助和章节查询，打包 smoke 检查内置手册。

发布前同步根包、桌面 Rust 包、Cargo.lock 本地包版本、desktop/package.json 与 Tauri 版本，并重新构建 CLI / App / helper。同一发布版本不能分发不同协议；Cloudflare Worker 按声明的协议范围对接设备。协议、签名格式或数据库结构改变时，发布说明须明确兼容范围及迁移要求，具体规则见下文。

## 真实 Cloudflare 集成测试

完整地址存入仓库之外的 0600 文件：

```bash
XRUN_TEST_CF_LINK_FILE=/绝对路径/私有地址文件 \
  cargo test --locked --test cloudflare -- --ignored --nocapture
```

测试用正式 CLI / daemon 创建隔离网络，覆盖任务、64 MiB 文件、流式执行、转发、管理设备离线、空闲恢复和撤销。本地 workerd 不能代替真实 Cloudflare 休眠验证。`cloudflare/tests/workerd/deployed-probe.ts` 仅用于临时部署；`cloudflare/scripts/hibernation.ts` 用静默前后的实例标识变化直接证明重建，并校验附件中的额度、期限和恢复后的 ACK。验收结束后清理临时 Worker / DO；探针不进入生产入口。子项目说明见 [cloudflare/README.md](../cloudflare/README.md)。

## 版本与兼容性

只修复实现或增加可安全忽略的诊断字段，提升发布版本即可。改变操作或执行选项的语义时，提升 `PROTOCOL`，实际实现相邻旧协议后调整支持范围；发送端通过 `Request::minimum_protocol` 与 `Session::send_request` 检查协商结果，接收端也检查。改变签名结构或编码时，提升 `SIGNATURE_FORMAT` 并明确重签 / 过渡方案，禁止因修改字段顺序而无意改变签名字节。固定测试向量位于 `tests/fixtures/signatures.json`，Rust 与 Cloudflare 的测试共同约束它。

Linux CI 的 `bun scripts/test-compatibility.ts` 按 SemVer 选择早于当前版本的最近兼容 tag，正式版优先于同版本预发布版。脚本构建真实历史 CLI 与 Worker，在两种来源 / 目标组合和两套中转上验证加入、执行、文件传输、清单同步和撤销。没有兼容 tag 时，固定使用 `bc97f764b4c96e07fab2736ee029349389eb0c03` 的协议 1 开发快照，并在日志中明确标为未发布快照，仍完整执行测试；缺失基线或损坏的协议定义会失败，不能跳过后报成功。beta.3 及更早版本不属于兼容范围。也可以显式传入兼容的 Git ref 做开发验收；开发快照验证不等于已发布版本兼容证据。

## 性能测量

共享 CI 运行已有会话的输出与授权失效功能测试，采用 5 秒等待上限，避免把机器调度抖动当作产品回归。100 ms 输出可见、200 ms 授权失效仍是受控空闲机器的验收预算，单独运行并保存实测报告：

```bash
XRUN_LATENCY_OUTPUT=target/latency.json \
  cargo test --locked --test latency -- --ignored --nocapture --test-threads=1
```

报告包含验收模式、预算和各项时延；功能 CI 的 5 秒结果不能作为上述时延预算通过的证据。[Readability](../.github/workflows/quality.yml) 单独检查源码长度、稠密代码与 Cloudflare 状态类型约束。

手动运行 [Measure performance](../.github/workflows/performance.yml)，选择基线 Git ref，获取 Linux、macOS、Windows 的空闲 CPU、日志写入和同期查询数据。两版分别构建测试入口和 CLI，使用隔离目录；发布号不同也能对照。原始优化前基线 `21a60c5` 尚无测量文件，脚本将最早的测量夹具编译到该基线的库上，并在报告中记录夹具来源；生产代码保持该 ref 的实现。时延是测量结果，不设 CI 吞吐阈值。

```bash
bun desktop/scripts/benchmark-core.ts a97ef65
XRUN_BENCH_LOG_MODE=bursts bun desktop/scripts/benchmark-core.ts a97ef65
# Linux：单独跟踪，跟踪期间的耗时不参与性能比较
XRUN_BENCH_TRACE=1 bun desktop/scripts/benchmark-core.ts a97ef65
```

结果默认写入 `output/benchmarks/`。Linux 文件读取和数据库同步调用按采样时间窗统计，原始跟踪不保留且不采集读取内容。同步调用包含任务状态提交，日志块数不等于 fsync 次数；macOS / Windows 没有读取与同步调用统计，Linux / Windows 没有唤醒次数统计。

`scripts/benchmark-cache.ts` 用三台独立目标和真实的 10、30、60 秒间隔测量缓存决策，需要设置 `XRUN_TEST_BINARY`、`XRUN_TEST_CF_LINK_FILE`、`XRUN_BENCH_CACHE_OUTPUT`。私有测试地址不写入报告；测试结束后停止 daemon 并删除隔离目录。
