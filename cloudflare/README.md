# Cloudflare 中转

与 Rust 中转共用设备连接认证和端到端会话协议。每个部署只服务一个网络，Worker
将请求送到固定的 Durable Object，持久保存网络与管理设备绑定，保存在线连接与临时配对关系并转发密文，按网络与设备持久化每日流量汇总。
成员清单、授权、撤销、任务、日志和文件由端点处理。

设备使用与中转操作见 [使用手册](../docs/usage.md#中转部署)，项目开发和发布说明见 [开发文档](../docs/development.md)。

## 部署

安装发布版 xrun 后执行：

```sh
xrun relay deploy --provider cloudflare --name xrun-relay
```

在浏览器登录 Cloudflare 并授权 xrun，CLI 直接上传内置的预编译 Worker。
支持 macOS、Windows 和 Linux，用户不需要源码、Bun、Node.js、Wrangler 或编译环境。
多账号可通过 `--account <账号 ID>` 选择；同名重复部署保留随机路由和 DO 数据。
`xrun relay login --provider cloudflare` 可提前授权，`xrun relay logout --provider cloudflare` 撤销本地登录且保留中转。
`xrun relay deploy --provider cloudflare --dry-run --json` 只检查包内产物。
`xrun relay link --provider cloudflare --name xrun-relay` 从本机记录查看部署地址，不联网。
部署记录与登录凭据在私有 `~/.xrun/cloudflare/` 中，应保留以便更新。

## 开发者源码部署

此入口需要源码、Bun 和已登录 Cloudflare 的 Wrangler。在项目根目录执行：

```sh
bun install --cwd cloudflare --frozen-lockfile
bun run --cwd cloudflare deploy --name xrun-relay
```

多账号时设置 `CLOUDFLARE_ACCOUNT_ID`。命令输出完整 HTTPS 地址，粘贴到 App，
或用于 `xrun up --relay '<完整地址>' --name mac1`。同名再次部署复用随机路由；
保留 `.deploy/` 中的私有状态文件，勿纳入 Git。中转地址需要保密。
部署和授权 Cloudflare 账号不会创建网络管理身份。你选定的本地设备首次创建网络后成为管理设备，
中转验证根密钥和设备签名后持久绑定它；普通探测、匿名连接和无管理签名的成员不能认领。
后续设备通过该管理设备发出的邀请加入；其他网络即使持有完整中转地址也会被拒绝。
绑定在 Worker 更新和 DO 恢复后保留，重复创建只允许原管理设备重试。
知道地址也需要设备证书和私钥才能占用在线路由；无证书请求只能向管理设备
发起端到端配对，仍需要有效成员邀请。

Cloudflare 使用公共 HTTPS 证书，内部继续使用网络自己的双向 TLS。
端点与 Worker 按协议范围协商；发布号用于诊断。部署写入代码支持的
`XRUN_PROTOCOL_MIN` / `XRUN_PROTOCOL_MAX`。协议 1 和独立签名格式 1 从
beta.4 开始，旧 beta 需要重新组网。

beta.5 支持协议 1–2，签名格式不变。协议 2 允许已认证来源显式声明缓存空闲；
每网络最多 8 个会话，每个已认证来源最多 4 个，匿名来源按 IP 最多 4 个。普通连接最多占 7 个，最后 1 个仅供已用根私钥证明的管理设备来源使用；匿名配对不能使用该预留名额。满额时只回收归还的空闲缓存，来源额度满时只回收该来源自己的缓存。重新激活必须
先获中转确认，正在执行或仅仅没有输出的会话不会被回收。协商版本与空闲
标记随 WebSocket 附件保存，休眠恢复后继续生效；新版 daemon 连接协议 1
中转时不保留空闲缓存。

当前支持协议 1–3。协议 3 增加中转归属探测与管理设备认领，新的创建流程要求协议 3；
已有管理设备连接尚未绑定的中转时也会自动完成签名认领，普通成员不能抢占绑定。

## 流量统计

`xrun traffic --period month --json` 查询本月流量，`today` 查询 UTC 今日，`all` 查询累计。
管理设备可读本网络总量与设备收发明细，其他成员只读自身。统计有效密文二进制帧，
外层认证、ACK 与心跳不计入；记录不因连接复用、断开或 DO 恢复清零。
没有流量额度和带宽策略。存储错误不会中断转发，但统计会标明不完整或返回存储错误。
完整口径和数据设计见 [流量统计](../docs/traffic.md)。

## 检查和测试

```sh
bun run --cwd cloudflare check
bun run --cwd cloudflare test
```

测试使用 workerd 验证 DO 接口、证书挑战、冒用拒绝、匿名配对限制、一次性接入、
断线清理和缓冲窗口。每方向最多 4 MiB 未确认密文，单帧最多 64 KiB；接收端
确认后释放窗口。每网络最多 8 个来源会话（含待接入），双向未确认密文预算
合计最多 64 MiB，此外仍有运行时和连接开销。中转不限制累计传输量，xrun 的单文件大小也不设固定上限。

真实部署集成测试：先把完整地址存入仓库之外的 0600 文件，再执行：

```sh
XRUN_TEST_CF_LINK_FILE=/绝对路径/私有地址文件 \
  cargo test --locked --test cloudflare -- --ignored --nocapture
```

测试创建隔离网络，使用正式 CLI 和 daemon，覆盖命令、任务历史、65 MiB 文件
往返、二进制流式输入、TCP 转发、管理设备离线、空闲恢复和撤销。
本地 workerd 不能代替真实 CF 的休眠验证。

## 管理和删除中转

通过原生 CLI 部署的中转可执行：

```sh
xrun relay remove --provider cloudflare --name xrun-relay --purge
```

只删除本机有记录、远端归属标记匹配的实例。`--purge` 显式确认永久删除
Worker、DO 命名空间和流量统计；删除失败可重试，重新部署请换一个名称。
`xrun relay logout --provider cloudflare` 只撤销登录，保留已部署的中转。
以下删除脚本仅适用于开发者源码部署、且 `.deploy/` 中保留了部署记录的实例：

```sh
bun run --cwd cloudflare remove --name xrun-relay
```

命令只接受本地有部署记录的名称；先删除该 Worker 的 DO 类和命名空间，再删除
Worker。清理后再次部署请换一个名称。

参考：[WebSocket 休眠](https://developers.cloudflare.com/durable-objects/best-practices/websockets/)、
[WebCrypto](https://developers.cloudflare.com/workers/runtime-apis/web-crypto/)、
[DO migration](https://developers.cloudflare.com/durable-objects/reference/durable-object-class-migrations-legacy/)。
