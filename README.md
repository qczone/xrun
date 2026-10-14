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

xrun 是面向开发者和 AI 的跨设备工具。用同一套命令，在自己的 macOS、Windows 和 Linux 设备上构建、测试、运行后台任务、传输文件、截图和转发端口。桌面 App 负责组网、授权、后台服务和活动记录，支持中英文和明暗主题。

```bash
xrun linux1 -C /home/user/demo -- cargo test       # 在 Linux 上运行测试
xrun win1 start -C 'D:\demo' -- cargo build        # 在 Windows 上启动后台构建
xrun win1 pull 'D:\demo\target\debug\app.exe' ./app.exe
xrun win1 screenshot ./screen.png
xrun linux1 forward 8080:3000                      # 在本机访问远端的开发服务
```

## 能做什么

- **跨平台执行**：在目标设备上运行程序或脚本，实时取回输出和退出码。
- **后台任务**：启动后随时查询状态、读取日志、等待完成或取消；断线不影响任务。
- **文件与截图**：传输单个文件，支持校验、续传和防覆盖；截取远端屏幕。
- **端口转发**：把远端的开发服务映射到本机浏览器。
- **交给 AI**：稳定的退出码、错误码和 JSON 输出；`xrun help` 和 `xrun doc` 离线可读。

目前不支持 PTY 终端、目录同步和离线任务排队。

## 开始使用

下面以 Mac `mac-pro` 管理 Linux `linux1` 为例，两台设备都需先安装 xrun。

1. 从 [Release](https://github.com/xrun-sh/xrun/releases) 下载对应平台的文件，或使用[命令行安装](docs/usage.md#命令行安装)。
2. 在 Mac 上部署中转，再用输出的完整地址创建网络，Mac 成为管理设备：

   ```bash
   xrun relay deploy --provider cloudflare --name xrun-relay
   xrun connect '<输出的中转地址>' --name mac-pro
   ```

3. 在 Mac 上运行 `xrun invite` 生成接入码，然后在 Linux 上加入，并允许 Mac 访问：

   ```bash
   xrun connect <中转地址> <接入码> --name linux1 --allow-manager
   ```

4. 回到 Mac，查看连接并运行第一条远程命令：

   ```bash
   xrun status
   xrun linux1 -- uname -s
   ```

   看到 `Linux` 且命令退出码为 0，就完成了一次远程执行。遇到问题见[状态与排错](docs/usage.md#状态与排错)。

桌面 App 的“新建网络”可以在界面中完成部署和创建。中转也可以自建在 Linux 主机上，见 [中转部署](docs/usage.md#中转部署)。

## 平台

| 平台 | 下载 |
| --- | --- |
| macOS 13+ · x86_64 / arm64 | App ZIP，自带 CLI |
| Windows · x86_64 / arm64 | 安装程序 EXE，包含 App 和 CLI |
| Linux · x86_64 / arm64 | CLI 压缩包 |

中转可以部署到 Cloudflare（Worker + Durable Object，在任意系统上用一条命令完成），也可以自建在 Linux 主机上。

## 安全

- **端到端加密**：设备之间在中转隧道内使用双向 TLS。中转保存网络归属、管理设备签名的成员清单（设备名称、密钥指纹和撤销状态）以及流量计数，看不到命令、输出、文件和截图。
- **加入不等于授权**：每台设备自己决定允许谁访问。被允许的设备拥有本机当前用户的全部权限，请只授权可信设备。
- **管理设备需要在线**：它负责邀请、撤销和续签成员有效期（默认 15 分钟，最长可设为 24 小时）。离线超过有效期后，新的远程操作会被拒绝，已开始的任务继续运行。

## 文档

- [使用手册](docs/usage.md)：安装、组网、授权、日常使用和排错，也可以用 `xrun doc` 离线阅读；AI 和脚本从 [`xrun doc automation`](docs/usage.md#自动化与-ai) 开始。
- [开发与发布](docs/development.md)：构建、测试、发布流程，以及架构、协议和兼容规则。

[MIT License](LICENSE)
