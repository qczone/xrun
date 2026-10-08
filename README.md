<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/logos/xrun-wordmark-dark.svg">
    <img src="assets/logos/xrun-wordmark-light.svg" alt="xrun" width="203" height="66">
  </picture>
</p>

<p align="center">
  <strong>在你的设备之间执行程序、管理任务和传输文件。</strong>
</p>

<p align="center">
  <a href="docs/usage.md#安装">安装</a> ·
  <a href="docs/usage.md#开始使用">开始使用</a> ·
  <a href="docs/usage.md">使用手册</a> ·
  <a href="docs/development.md">开发与发布</a>
</p>

xrun 是面向开发者和 AI 的跨设备工具。通过同一套 CLI，使用已授权的 macOS、Windows 和 Linux 设备完成构建、测试、后台运行、文件传输、端口转发和截图。桌面 App 提供网络、访问权限、后台服务与本机活动的管理界面，支持中英文和明暗主题。

## 能做什么

- **跨平台执行**：从 Mac 调用 Windows 构建程序，或在 Linux 上运行测试并接收输出和退出结果。
- **管理后台任务**：启动远端程序，随后查询状态、读取日志、等待完成或取消执行。
- **传输文件**：取回构建产物和报告，在本地编辑文件后再上传。
- **检查远端界面与服务**：截取远端屏幕，或把远端开发服务的端口转发到本机浏览器。
- **交给 AI 操作**：通过 CLI、稳定错误码和 JSON 结果决定下一步；帮助与完整手册均可离线读取。

设备加入网络并授权后，可以从 Mac 发起：

```bash
xrun linux1 -C /home/user/demo -- cargo test
xrun win1 start -C 'D:\demo' -- cargo build
xrun win1 screenshot ./screen.png
xrun linux1 forward 8080:3000
```

任务、输出日志和文件操作记录保存在目标设备，桌面 App 可以查看本机记录。当前文件传输以单文件为单位，截图覆盖主显示器；尚不支持 PTY 终端、目录同步和离线任务排队。

## 连接与授权

所有设备主动连接同一个中转，无需向设备开放入站端口。中转可选择自建 Linux Rust 服务或 Cloudflare Worker + Durable Object，负责接通连接、转发设备间的端到端加密数据，不保存成员清单、任务、日志或文件。

创建网络的设备是管理设备，负责签发邀请和撤销成员。**加入网络不等于获得访问权限**：目标设备单独决定允许谁访问本机，所有操作以目标设备的当前用户权限运行。

加入网络时管理设备需要在线；已加入设备的日常执行只需要中转、双方在线且身份和授权有效。

## 平台与安装

当前版本为 `0.1.0-rc.2`。

| 平台 | CLI | 桌面 App |
| --- | --- | --- |
| macOS 13+ · x86_64 / arm64 | 压缩包 | DMG / App ZIP |
| Windows · x86_64 / arm64 | 压缩包 | 当前用户安装程序 |
| Linux · x86_64 / arm64 | 压缩包 | — |

每种系统分别提供两种架构的包：`x86_64` 即 AMD64，`arm64` 对应 Apple Silicon、Windows ARM 和 ARM Linux。

CLI、设备后台服务和 Linux Rust 中转使用同一个二进制。运行发布包不需要安装 Rust、Node.js 或 Bun。

[查看安装说明](docs/usage.md#安装)。macOS 和 Windows 另提供适合 AI 与自动化的安装脚本，支持指定版本、SHA-256 校验和 JSON 结果，详见 [自动安装](docs/usage.md#ai-和命令行自动安装)。

安装 CLI 后，`xrun help` 查看简洁帮助，`xrun doc` 查看完整离线手册，`xrun doc --list` 列出章节。内置手册与仓库使用同一份文件，随程序发布。

## 文档

| 文档 | 内容 |
| --- | --- |
| [使用手册](docs/usage.md) | 安装、加入、授权、执行、任务、文件、服务与排错 |
| [开发与发布](docs/development.md) | 源码构建、检查、测试、打包和发布产物 |
| [设计文档](docs/design.md) | 架构、协议、行为契约与验收边界 |
| [Cloudflare 中转](cloudflare/README.md) | Worker 开发、部署和测试 |

## 开发

核心使用 Rust，桌面端使用 Tauri 2、React 和 TypeScript，Web 脚本与测试使用 Bun。构建与检查使用仓库固定的工具链，详见 [开发说明](docs/development.md)。

[MIT License](LICENSE)
