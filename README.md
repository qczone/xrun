# xrun

xrun 是自托管的跨设备命令执行工具。CLI 和 Agent 使用同一个二进制，设备主动连接 Server；代码由使用方通过 Git 分支同步，xrun 不同步文件。

当前实现使用 Rust 2024 edition。Server 面向 Linux 部署；CLI 和 Agent 包含 macOS、Linux、Windows 路径。已在 macOS 上完成本机双设备端到端测试，并通过 Linux GNU 与 Windows GNU 目标交叉类型检查；Linux 和 Windows 的实机运行仍需验收。

## 构建与部署

```bash
cargo build --release
```

GitHub Actions 的 `Package` 工作流可手动触发，分别构建 Linux x86_64、Windows x86_64 和 macOS Apple Silicon 压缩包。macOS 可执行文件使用 Developer ID 签名，并在 Apple 公证通过后上传。每个压缩包包含可执行文件、许可证和 README，可从对应工作流的 Artifacts 下载。

在 Server 主机创建配置，例如 `server.toml`：

```toml
listen = "0.0.0.0:7443"
public_url = "https://203.0.113.10:7443"
data_dir = "/var/lib/xrun"
```

把示例 IP 换成实际公网 IP，开放对应 TCP 端口，然后运行：

```bash
xrun server --config server.toml
xrun pair --config server.toml
```

Server 自建 CA 和 HTTPS 证书。`pair` 通过仅本机可访问的 Unix socket 创建一次性配对链接；链接包含 Token、CA 指纹和 CA 证书，必须经可信渠道交给设备。Token 有效期 10 分钟。

在每台设备上分别配对：

```bash
xrun join '<配对链接>' --name mac1
```

配对后显示不可变的设备 ID。目标设备在 `~/.xrun/agent.toml` 中配置允许的来源设备 ID，默认空列表表示拒绝所有远程执行：

```toml
allow_from = ["dev_来源设备ID"]
max_concurrent_jobs = 4

[env]
PATH = "/usr/local/bin:/usr/bin:/bin"
```

然后在目标设备运行 `xrun agent`。首次启动自动建立本地任务数据库；数据库丢失后不会静默重建。Agent 以前台运行，生产环境可自行配置系统服务。Server 主机若也要接受命令，须像普通设备一样配对并运行 Agent。

## 使用

```bash
xrun ls
xrun info mac1
xrun exec mac1 -C /path/to/repo -- cargo test
xrun mac1 -C /path/to/repo -- git status --short
xrun exec mac1 --stdin -- git apply - < change.patch
xrun exec mac1 --detach --timeout 3600 -- cargo build
xrun jobs --device mac1
xrun job <job-id>
xrun logs <job-id> --follow
xrun kill <job-id>
```

`exec` 直接传递程序和参数，不会隐式启动 Shell。需要管道或重定向时，显式执行远端 Shell。`--stdin` 最多接收 1 MiB，输入必须来自管道或重定向。`--env KEY=VALUE` 可重复指定；`--request-id` 用于重试同一提交，参数不同会返回 `REQUEST_CONFLICT`。`--json` 输出机器可读结果；前台正常退出时透传 `0..255` 的远端退出码，执行层错误返回 `125`。

证书临近到期时 CLI 和 Agent 会通过已认证连接续期，也可运行 `xrun renew`。长期离线且证书已过期时，在 Server 主机运行 `xrun pair --config server.toml --renew mac1`，再在原设备执行 `xrun join '<恢复链接>'`。撤销身份使用 `xrun revoke --config server.toml mac1`。

任务及完整日志保存在目标 Agent；Server 缓存最近的有限输出。目标离线时若缓存缺失，CLI 会明确报告 `LOG_UNAVAILABLE`。Server 和 Agent 均使用 SQLite WAL；Agent 的数据库和私钥都在 `~/.xrun`。详细协议与故障语义见 [design.md](design.md)。

当前验收仍缺 Linux/Windows 实机测试、设备睡眠与存储故障注入。不要仅凭交叉编译结果将这两端视为已完成运行时验收。
