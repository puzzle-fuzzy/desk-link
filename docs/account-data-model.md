# DeskLink 账号数据模型

账号数据库位于 `server/account/src/db/migrations/001_account.sql`，当前使用 SQLite。它是应用登录控制面，不是远程连接凭据库。

| 表 | 保存内容 | 明确不保存 |
| --- | --- | --- |
| `users` | 规范化邮箱、Argon2id 密码哈希、验证时间、状态 | 明文密码、邮箱验证 token |
| `account_devices` | 设备 ID、平台、名称、最近登录时间、撤销时间 | 已保存主机、relay 密钥、主机权限 |
| `account_sessions` | access/refresh 哈希、session family、过期/轮换/撤销状态 | 明文 bearer token |
| `action_tokens` | 邮箱验证/密码重置 token 哈希、用途、过期和消费时间 | 可再次使用的原文 token |
| `audit_events` | 脱敏动作、用户/设备 ID、时间和受限元数据 | 密码、token、远程密钥、完整请求体 |
| `email_outbox` | 收件人、邮件模板、action token 记录 ID、重试时间和错误摘要 | 邮件正文中的原始 token、provider bearer token |

## 关键不变量

- `users.email` 唯一且在写入前统一为小写、去首尾空格。
- 账号设备 ID 在一个时刻只属于一个账号。退出后设备可以在下一次登录时重新登记，但不会带回旧账号的远程连接。
- 一个设备的新登录会撤销该设备旧的应用 session；其他设备的 session 保持有效。
- refresh token 轮换必须在 SQLite 事务中完成。重复使用旧 refresh token 会撤销整个 family。
- action token 必须匹配用途、未消费且未过期，并在同一事务中标记消费。
- 密码重置会撤销该账号的全部 session，但不删除远程主机的可信控制端。
- 邮箱找回和重复注册使用不暴露账号存在性的统一成功语义。

## 迁移、备份和扩展

当前启动时执行幂等 SQL 迁移，生产环境限制为单个 Bun 进程共享一个 SQLite 文件。备份必须覆盖数据库文件及其 WAL 状态，并在恢复后通过 `/ready` 和受控冒烟测试验证。

当需要多实例、跨区域或高并发邮件队列时，不应继续把 SQLite 文件挂到多个容器；应迁移到 PostgreSQL，并把 outbox 改为带租约的队列消费者。
