# 开发与发布

项目介绍见 [README](../README.md)，日常使用见 [使用手册](usage.md)，架构、协议与验收边界见 [设计文档](design.md)。

## 构建

先安装 rustup。项目通过 [rust-toolchain.toml](../rust-toolchain.toml) 固定 Rust 版本及 rustfmt、clippy 组件，本地构建、三平台 CI 和打包使用同一工具链；在项目目录运行 Cargo 时由 rustup 自动选择。

```bash
cargo build --locked --release
```

将 `target/release/xrun`（Windows 为 `xrun.exe`）放到固定目录并加入 PATH，再安装服务。CLI、daemon 和 Linux Rust 中转使用同一个二进制；Cloudflare 中转单独部署。网络组件按支持的协议范围协商，本机 CLI / App helper 与 daemon 要求发布版本一致。发布要求见 [design.md 的版本说明](design.md#63-版本和限制)。

GitHub Actions 的 `Package` 工作流可手动构建三个平台的 CLI 压缩包，以及 macOS Apple Silicon DMG、App ZIP 和 Windows x86_64 用户级 NSIS 安装包；macOS App 和 DMG 的签名、公证需要配置工作流列出的凭据。每个平台的产物还包含版本和 SHA-256 清单，macOS、Windows 同时提供自动安装脚本。

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
| macOS | DMG，或包含完整 App 的 ZIP | `target/release/bundle/dmg/` 下的 `.dmg`，`target/release/bundle/macos/xrun.app.zip` |
| Windows | 当前用户的 NSIS 安装程序 | `target/release/bundle/nsis/` 下的安装 `.exe` |

macOS 构建同时保留 `target/release/bundle/macos/xrun.app`；签名检查通过后，用 `ditto` 将同一份 App 打包为 ZIP。正式工作流会先完成 App 公证及票据附加，再生成 ZIP。

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

## 验证

Rust 模块内部测试统一放在对应源码末尾的 `#[cfg(test)] mod tests` 中，平台限定条件按需保留。集成测试放在 `tests/`。桌面端原生 UI 测试需要在进程主线程运行，独立入口保留在 `desktop/src-tauri/tests/native_ui.rs`，使用 `harness = false`。

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
# Linux X11 截图测试（需安装 Xvfb、libX11、libXrandr）
xvfb-run -a -s '-screen 0 1024x768x24' cargo test --locked --test screenshot -- --ignored
```

[Test Linux](../.github/workflows/test-linux.yml)、[Test macOS](../.github/workflows/test-macos.yml) 和 [Test Windows](../.github/workflows/test-windows.yml) 分别在对应平台原生运行核心测试，覆盖配对、授权、执行、任务、文件传输、流式执行、端口转发、会话缓存和故障恢复。测试直接调用 Rust 中转库，`xrun relay` 部署命令仍仅支持 Linux。Linux 工作流另运行 Xvfb 截图测试和 Cloudflare workerd 测试；macOS、Windows 还检查桌面端、安装包内的 helper 和自动安装脚本，覆盖中文及空格路径、重复安装、损坏产物、版本不符，以及 macOS 回滚和 Windows 静默升级／卸载失败。每次提交是否通过，以对应的 Actions 结果为准。

Cloudflare 中转的本地检查无需云端凭证：

```bash
bun run --cwd cloudflare check
bun run --cwd cloudflare test
```

Ubuntu 26.04 x86_64 云主机与 macOS ARM64 本机已通过公网 TCP 8080 实机检查：双向执行与文件传输、任务去重/等待/取消、授权、繁忙重试、中转中断后的日志恢复，以及 systemd 和 LaunchAgent 的异常重启。另已通过真实 Cloudflare 中转验证独立 CLI 进程间的会话复用、64 MiB 文件往返、空闲恢复和撤销。云端无桌面时截图返回 `NO_DISPLAY` 和退出码 1。

系统服务、真实桌面截图权限/锁屏、设备睡眠和磁盘故障的验收进度见 [design.md](design.md)。

## 发布与内置文档

Package 工作流只生成 Actions 附件，不创建 Release。对外发布时将附件上传到 v<完整版本> Release。产物清单由 [package-manifest.ts](../desktop/scripts/package-manifest.ts) 使用现有 Bun 生成；用户运行安装程序不需要 Bun，见 [安装说明](usage.md#安装)。

CLI 压缩包包含 LICENSE、README、docs/ 和 scripts/ 下的安装脚本，使用手册中的相对链接在压缩包中可用；开发文档引用的源码和 CI 文件在仓库中查看。`docs/usage.md` 编译进 CLI，随程序发布，仓库、附件和离线 `xrun doc` 使用同一份手册；更新后需要重新构建。文档测试验证无身份、无网络的帮助和章节查询，打包 smoke 检查内置手册。

发布前同步根包、桌面 Rust 包、Cargo.lock 本地包版本、desktop/package.json 与 Tauri 版本。协议变化需要新完整版本，同一版本不能分发不同协议；重新构建 CLI / App / helper，并部署同版本 Cloudflare Worker。当前持久化签名无跨版本迁移，发布说明应明确数据兼容和重新组网要求。

## 真实 Cloudflare 集成测试

完整地址存入仓库之外的 0600 文件：

```bash
XRUN_TEST_CF_LINK_FILE=/绝对路径/私有地址文件 \
  cargo test --locked --test cloudflare -- --ignored --nocapture
```

测试用正式 CLI / daemon 创建隔离网络，覆盖任务、64 MiB 文件、流式执行、转发、管理设备离线、空闲恢复和撤销。本地 workerd 不能代替真实 Cloudflare 休眠验证。`cloudflare/tests/workerd/deployed-probe.ts` 仅用于临时部署；`cloudflare/scripts/hibernation.ts` 用静默前后的实例标识变化直接证明重建，并校验附件中的额度、期限和恢复后的 ACK。验收结束后清理临时 Worker / DO；探针不进入生产入口。子项目说明见 [cloudflare/README.md](../cloudflare/README.md)。

## 版本与兼容性

只修复实现或增加可安全忽略的诊断字段，提升发布版本即可。改变操作或执行选项的语义时，提升 `PROTOCOL`，实际实现相邻旧协议后调整支持范围；发送端通过 `Request::minimum_protocol` 与 `Session::send_request` 检查协商结果，接收端也检查。改变签名结构或编码时，提升 `SIGNATURE_FORMAT` 并明确重签 / 过渡方案，禁止因修改字段顺序而无意改变签名字节。固定测试向量位于 `tests/fixtures/signatures.json`，Rust 与 Cloudflare 的测试共同约束它。

Linux CI 的 `bun scripts/test-compatibility.ts` 选择最近的兼容发布 tag，构建真实历史 CLI 与 Worker，在两种来源 / 目标组合和两套中转上验证加入、执行、文件传输、清单同步和撤销。协议 1 的首版没有兼容历史 tag，只报告初始化基线；beta.3 及更早版本不属于兼容测试范围。也可以显式传入兼容的 Git ref 做开发验收；这不等于已发布版本兼容证据。首次签名 / 协议切换不迁移历史网络。
