# xrun

xrun 让开发者和 AI 在已授权的 macOS、Windows、Linux 设备上执行程序、管理后台任务、传输文件、转发开发服务端口和截图。

设备主动连接自建 Linux Rust 中转或 Cloudflare 中转，无需向客户端开放入站端口。设备之间端到端加密；目标设备自己决定谁能以当前用户权限访问本机。

## 适合哪些场景

- 从 Mac 驱动 Windows 或 Linux 完成构建、测试和运行，取回结果。
- 启动远端后台程序，查询任务、读取日志或取消执行。
- 下载文件，在本地编辑后上传；通过 Git 同步项目代码。
- 把远端开发服务转发到本机浏览器，或截图检查图形界面。
- 让 AI 使用同一套 CLI 操作已授权设备，依据退出码和 JSON 结果判断下一步。

```bash
xrun linux1 -C /home/user/demo -- cargo test
xrun win1 start -C 'D:\demo' -- 'target\debug\demo.exe'
xrun win1 screenshot ./screen.png
xrun linux1 forward 8080:3000
```

## 组成与平台

| 组件 | 平台与职责 |
| --- | --- |
| CLI 和 daemon | macOS、Windows、Linux；同一个 Rust 二进制发起调用或接受已授权请求 |
| 桌面 App | macOS 13+、Windows；管理网络、授权、后台服务及本机活动记录 |
| 中转 | Linux Rust 服务，或 Cloudflare Worker + Durable Object；接通连接并转发密文 |
| 管理设备 | 创建网络的设备；签发成员身份和邀请，撤销成员 |

日常执行不要求管理设备在线，加入网络需要管理设备在线。成员身份与访问授权分开，加入后默认不能控制其他设备。

任务、输出日志和文件操作记录保存在目标设备。中转不保存成员清单、任务、日志或文件。当前支持单文件传输和主显示器截图，不提供交互终端、目录同步或离线任务排队。

## 安装与文档

当前版本为 `0.0.1-beta.4`。macOS Apple Silicon 提供 DMG / App ZIP，Windows x86_64 提供用户级安装程序；CLI 提供三个平台的压缩包，其中 Linux 为 x86_64。安装包和自动安装脚本的使用见 [安装说明](docs/usage.md#安装)。

- [完整使用手册](docs/usage.md)：安装、加入、授权、执行、任务、文件、服务与排错。
- [开发与发布](docs/development.md)：源码构建、检查、测试、打包和发布产物。
- [设计文档](docs/design.md)：架构、协议、行为契约与验收边界。
- [Cloudflare 子项目](cloudflare/README.md)：Worker 开发、部署和测试。

安装 CLI 后，`xrun help` 查看简洁帮助，`xrun doc` 查看完整离线手册，`xrun doc --list` 列出章节。内置手册与仓库使用同一份文件，随程序发布。

## 开发

核心使用 Rust，桌面端使用 Tauri 2、React 和 TypeScript，现有 Web 脚本与测试使用 Bun。源码构建和三平台检查使用仓库固定的 Rust 工具链，详见 [开发说明](docs/development.md)。

许可证见 [LICENSE](LICENSE)。
