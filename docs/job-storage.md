# 统一 Job 存储

所有业务操作由目标设备接受为一个 Job：`exec`、`stream_exec`、`push`、`pull`、`screenshot`、`forward`。查询、状态轮询和取消不新建 Job。桌面活动旅程只展示其他设备在本机执行的操作，直接读取本机 `daemon.db`，不向其他设备查询。

唯一建表定义是 [src/store/schema.sql](../src/store/schema.sql)。daemon schema 为 2，直接初始化新结构，不迁移或回填旧记录、不保留旧接口。遇到更旧的活动库或提交记录库，先用 SQLite 完整备份（包含 WAL 中已提交的数据）到私有 `database-backups/`，再在事务中建立新结构；备份或建表失败保留原库。未知的较新 schema、非空且无版本的库以及损坏的库仍明确报错。数据库使用 WAL、FULL 持久化和外键约束。

桌面 App 打开时完成这一流程，活动查询等待准备完成。原来运行的旧 daemon 先正常停止、清理操作，再换用当前 App 的 CLI helper 并恢复运行；原来停止的服务保持停止。重启意图保存在私有标记中，失败或退出后可以重试。设备身份、网络成员清单和授权配置保留。旧记录只存在备份中，不显示在新活动旅程；备份不参与附件保留期限的自动清理。

## jobs

每项操作只有一条权威记录，公共字段单独存列，类型参数与结果使用 JSON；JSON 不重复保存状态、时间、身份、计数或附件。

| 字段 | SQLite 类型 | 含义 |
| --- | --- | --- |
| job_id | TEXT PRIMARY KEY | 本机六字符 Job ID |
| source_device_id | TEXT NOT NULL | 发起设备身份 |
| target_device_id | TEXT NOT NULL | 执行设备身份 |
| request_id | TEXT NOT NULL | 来源设备生成的去重键 |
| request_hash | TEXT NOT NULL | 操作类型与不可变参数的 SHA-256 |
| kind | TEXT NOT NULL | 六种业务类型之一 |
| params_json | TEXT NOT NULL | 类型专属参数，必须是合法 JSON |
| result_json | TEXT | 类型专属结果，未确定时为空 |
| state | TEXT NOT NULL | accepted/running/succeeded/failed/canceled/timed_out/lost |
| error_code | TEXT | 机器可读错误码 |
| error_message | TEXT | 最多 1024 UTF-8 字节的诊断 |
| created_at_ms | INTEGER NOT NULL | 不可变受理时间，时间线排序依据 |
| started_at_ms | INTEGER | 实际开始时间 |
| finished_at_ms | INTEGER | 最终状态持久化时间 |
| updated_at_ms | INTEGER NOT NULL | 最近状态或日志更新 |
| process_json | TEXT | 命令 PID、boot_id、进程启动标识 |
| leftover_possible | INTEGER NOT NULL | 无法确认进程清理时为 1 |
| last_log_seq | INTEGER NOT NULL | 已分配的最高日志序号，清理日志不回退 |
| log_bytes | INTEGER NOT NULL | 当前留存日志字节数 |
| output_complete | INTEGER | exec 输出是否完整；不留存输出的类型为空 |
| output_loss_reason | TEXT | 累积输出丢失原因，最多 1024 字节 |

唯一约束为 `(source_device_id, request_id)`。数据库代次 `db_id` 存在 meta，在协议和只读快照中附加到 Job，不在每行重复存储。

索引覆盖 `(created_at_ms DESC, job_id DESC)` 时间线、来源时间线、accepted/running 活动操作、失败时间线以及有日志的已结束 Job。命令并发只统计 exec/stream_exec 的 accepted/running 行；文件与转发分别使用原有并发名额。

| kind | params_json | result_json |
| --- | --- | --- |
| exec | program、args、cwd、timeout、shell、input_size、input_sha256 | exit_code、signal、duration_ms；流计数为空 |
| stream_exec | program、args、cwd、timeout；输入摘要为空 | exit_code、signal、duration_ms、input_bytes、stdout_bytes、stderr_bytes |
| push | path、cwd、size、sha256、mkdir、no_overwrite、expect | 最终 path、size、sha256、attachment_error |
| pull | path、cwd | 最终 path、size、sha256、attachment_error |
| screenshot | 空对象 | captured_at、width、height、size、sha256、attachment_error |
| forward | port | port、duration_ms、input_bytes、output_bytes |

命令环境覆盖值和 stdin/脚本正文不写入 Job 参数；请求摘要仍包含实际环境与输入摘要，以区分不同意图。文件和图片正文只存私有附件文件，转发与流式输出只记录字节数。

## job_logs

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| job_id | TEXT NOT NULL | 外键 jobs.job_id，删除 Job 时级联 |
| seq | INTEGER NOT NULL | 从 1 递增，与 job_id 组成主键 |
| stream | TEXT NOT NULL | stdout/stderr |
| bytes | BLOB NOT NULL | 原始输出字节 |

只为可靠 exec 留存日志。日志行、最高序号、Job 字节数和全局字节数在同一事务提交。单 Job 上限 64 MiB，全局 1 GiB；全局满时优先清理最早结束 Job 的日志，不清理运行中的日志。已结束日志按 finished_at_ms 保留 7 天。清理保留 Job、去重键和最高序号，标记 LOG_EXPIRED；截断标记 TRUNCATED。

## job_attachments

| 字段 | 类型 | 含义 |
| --- | --- | --- |
| attachment_id | TEXT PRIMARY KEY | 与原始路径无关的私有缓存 ID |
| job_id | TEXT NOT NULL | 外键 jobs.job_id，删除 Job 时级联元数据 |
| name | TEXT NOT NULL | 另存为建议文件名 |
| size_bytes | INTEGER NOT NULL | 独立副本的字节数 |
| sha256 | TEXT NOT NULL | 副本的校验值 |
| created_at_ms | INTEGER NOT NULL | 副本生成时间 |
| status | TEXT NOT NULL | available/expired/missing |
| deleted_at_ms | INTEGER | 到期副本清理时间 |

索引按 job_id 查询附件，以及按 status/created_at_ms 清理附件。push/pull 从已校验的临时快照复制，截图复制实际发送的 PNG。先持久保存副本，再保存外键元数据；启动前清理没有元数据引用的缓存副本。

副本位于 `attachments/<时间>_<随机ID>.blob`，独立于原文件。默认保留 30 天，`attachment_retention_days` 可设 0–3650，0 为长期保留。每 10 分钟检查，缩短期限立即清理；过期后保留元数据与活动摘要。预览或导出再次检查当前期限和可用性。附件留存失败不会把成功的业务结果改成失败，错误记录在该 Job 的 result 中。

## meta

| key | value | 含义 |
| --- | --- | --- |
| db_id | TEXT | 随数据库重建生成的新代次 |
| log_bytes | TEXT | 全局当前日志字节数 |

## 生命周期和查询

受理事务先去重，再检查配额并保存 accepted；执行只更新原 Job。非零命令退出是 failed，原始退出码和信号仍在 result。最终状态不能被迟到结果改写。push 在实际发布文件后记录 succeeded，即使等待响应的连接已经断开。

所有业务请求携带 request_id/db_id。相同来源、ID、摘要返回同一 Job；不同参数返回 REQUEST_CONFLICT，数据库重建返回 DB_RESET。连接绑定请求先返回 accepted/fresh，再传输业务数据；fresh=false 只返回已有记录，不重开文件、进程或 TCP 流。

exec 在客户端断线后继续运行。stream_exec 和 forward 连接绑定，kill 更新原 Job；流式进程在清理完成后保存结果，即使退出响应丢失也可查询。无法确定结果时是 lost。重启只清理具有身份凭据的旧进程，再将未完成 Job 标记 lost，不自动重新执行任何类型。

时间线按 created_at_ms/job_id 倒序，50 条一页；游标包含 db_id。Job 完成、日志追加或附件到期不会改变受理时间，因此不会将老操作移动到最新位置。running 筛选 accepted/running，failed 筛选 failed/canceled/timed_out/lost。

调用方的 `submissions.sqlite` 独立保存所有类型的 request_id、来源/目标、网络根、db_id、request_hash、kind、label、提交时间和已知 job_id，先记录再发送。`recent` 展示最近 24 小时的提交，记录保留 7 天；它不是活动记录的第二份数据来源。
