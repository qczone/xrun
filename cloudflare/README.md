# Cloudflare 中转

与 Rust 中转共用设备连接认证和端到端会话协议。Worker 负责入口，每个网络
对应一个 Durable Object，保存在线连接与临时配对关系并转发密文。
成员清单、授权、撤销、任务、日志和文件由端点处理。

设备使用与中转操作见 [使用手册](../docs/usage.md#中转部署)，项目开发和发布说明见 [开发文档](../docs/development.md)。

## 部署

本机需要 Bun，并已通过 Wrangler 登录 Cloudflare。在项目根目录执行：

```sh
bun install --cwd cloudflare --frozen-lockfile
bun run --cwd cloudflare deploy --name xrun-relay
```

多账号时设置 `CLOUDFLARE_ACCOUNT_ID`。命令输出完整 HTTPS 地址，粘贴到 App，
或用于 `xrun up --relay '<完整地址>' --name mac1`。同名再次部署复用随机路由；
保留 `.deploy/` 中的私有状态文件，勿纳入 Git。中转地址需要保密。
知道地址也需要设备证书和私钥才能占用在线路由；无证书请求只能向管理设备
发起端到端配对，仍需要有效成员邀请。

Cloudflare 使用公共 HTTPS 证书，内部继续使用网络自己的双向 TLS。
端点与 Worker 按协议范围协商；发布号用于诊断。部署写入代码支持的
`XRUN_PROTOCOL_MIN` / `XRUN_PROTOCOL_MAX`。协议 1 和独立签名格式 1 从
beta.4 开始，旧 beta 需要重新组网。

## 检查和测试

```sh
bun run --cwd cloudflare check
bun run --cwd cloudflare test
```

测试使用 workerd 验证 DO 接口、证书挑战、冒用拒绝、匿名配对限制、一次性接入、
断线清理和缓冲窗口。每方向最多 4 MiB 未确认密文，单帧最多 64 KiB；接收端
确认后释放窗口。每网络最多 8 个来源会话（含待接入），双向未确认密文预算
合计最多 64 MiB，此外仍有运行时和连接开销。中转不限制累计传输量，单文件仍遵守 xrun 的 64 MiB 上限。

真实部署集成测试：先把完整地址存入仓库之外的 0600 文件，再执行：

```sh
XRUN_TEST_CF_LINK_FILE=/绝对路径/私有地址文件 \
  cargo test --locked --test cloudflare -- --ignored --nocapture
```

测试创建隔离网络，使用正式 CLI 和 daemon，覆盖命令、任务历史、64 MiB 文件
往返、二进制流式输入、TCP 转发、管理设备离线、空闲恢复和撤销。
本地 workerd 不能代替真实 CF 的休眠验证。

## 删除自己部署的中转

```sh
bun run --cwd cloudflare remove --name xrun-relay
```

命令只接受本地有部署记录的名称；先删除该 Worker 的 DO 类和命名空间，再删除
Worker。清理后再次部署请换一个名称。

参考：[WebSocket 休眠](https://developers.cloudflare.com/durable-objects/best-practices/websockets/)、
[WebCrypto](https://developers.cloudflare.com/workers/runtime-apis/web-crypto/)、
[DO migration](https://developers.cloudflare.com/durable-objects/reference/durable-object-class-migrations-legacy/)。
