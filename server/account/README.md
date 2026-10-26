# DeskLink 账号服务

这是 DeskLink 的独立应用账号控制面，负责邮箱注册、邮箱验证、登录、刷新、退出登录和找回密码。它不参与远程设备配对、主机审批、Noise 握手或 relay 数据转发。

## 本地运行

```sh
cd server/account
bun install --frozen-lockfile
bun test
bun run typecheck
DESKLINK_ACCOUNT_PORT=3412 bun run start
```

开发环境默认使用内存 SQLite 和 `DevMailer`，邮件不会发到外部，只保存在进程内测试出口。生产环境禁止使用内存数据库，也禁止使用开发邮件出口。

## 配置

| 变量 | 说明 |
| --- | --- |
| `NODE_ENV` | `production` 启用生产强制检查；默认是 `development` |
| `DESKLINK_ACCOUNT_ADDR` | 监听地址；容器内使用 `0.0.0.0` |
| `DESKLINK_ACCOUNT_PORT` | 监听端口，默认 `3412` |
| `DESKLINK_ACCOUNT_DATABASE` | SQLite 文件路径；生产环境必填，建议挂载到独立持久卷 |
| `DESKLINK_ACCOUNT_ORIGIN` | 邮件验证和重置密码链接的公网 HTTPS 根地址，例如 `https://account.example.com` |
| `DESKLINK_ACCOUNT_CORS_ORIGINS` | 可选，逗号分隔的浏览器来源白名单；原生客户端不依赖 CORS |
| `DESKLINK_ACCOUNT_MAIL_URL` | 生产邮件 provider 的 HTTPS API 地址 |
| `DESKLINK_ACCOUNT_MAIL_TOKEN` | provider bearer token；只存在服务端环境变量，不写入数据库 |
| `DESKLINK_ACCOUNT_MAIL_FROM` | 发件人地址 |
| `DESKLINK_ACCOUNT_TRUSTED_PROXIES` | 默认空；逗号分隔的直属可信代理精确 IP。只有这些 socket peer 可提供代理覆盖的单值 `X-Real-IP`，不读取 XFF / CF 头作为身份。反代应覆盖该头且阻断外部直连；直连部署保持为空。 |

邮件 provider 接收的请求格式为：

```json
{
  "from": "DeskLink <noreply@example.com>",
  "to": ["user@example.com"],
  "subject": "验证你的 DeskLink 邮箱",
  "text": "...",
  "html": "..."
}
```

请求使用 `Authorization: Bearer <DESKLINK_ACCOUNT_MAIL_TOKEN>`。注册、重发验证、找回密码只在事务内写入 action token、审计和 SQLite outbox，提交后返回通用成功响应，不等待 provider。独立 worker 每次认领一封后发送，空闲轮询间隔 1 秒；保留 single-flight、最多 10 次和指数退避（10 秒起，最高 2560 秒）。outbox 仅保存模板、token 记录 ID、状态和固定错误类别，原始链接 token 只在投递内存中生成，不写入数据库。

邮件状态为 `pending` / `delivering` / `delivered` / `failed`。认领原子递增 attempts，持有 30 秒租约（HTTP provider 超时 10 秒），完成更新校验租约所有权和有效期。进程崩溃后自动恢复到期租约；第 10 次发送失败或崩溃后到期都进入明确终态。多个本地 SQLite 连接不会同时认领有效租约；provider 已收信但本地未记成功时可能重投，属于至少一次语义。每次重试替换 token，旧链接失效。SIGINT/SIGTERM 停止 HTTP 接收、定时器和新认领，等待当前投递结束后关库。

失败任务只有本地显式重投才会重新进入队列，并记录审计（没有公开 HTTP 重投接口）：

```sh
bun run src/outbox-admin.ts /absolute/path/account.sqlite list-failed
bun run src/outbox-admin.ts /absolute/path/account.sqlite retry <outbox-id>
```

查询仅输出最多 100 条任务 ID、状态、次数、时间和固定错误类别；重投重置该任务次数，不立即调用 provider。操作前确认目标是所需数据库。生命周期迁移 v3 将旧的 10 次失败记录转换为 `failed`，旧已发送记录转换为 `delivered`；当前 schema 为设备隔离版本 4。

## HTTP 检查

- `GET /health`：进程存活检查，返回 `status: ok`。
- `GET /ready`：校验 schema 版本和请求所需列，并通过回滚的短写事务验证数据库可写；SQLite 锁等待预算为 25ms，失败返回 503 / `not_ready`。输出仅含固定依赖状态，不返回路径、SQL、凭据或堆栈。`dependencies.mail: degraded` 表示存在邮件失败待处理，数据库仍健康时保持 200；探测不访问邮件 provider。

生产环境只应通过反向代理暴露 HTTPS。Bun 将 socket peer 交给 app；直连按 peer 限流。显式配置可信代理时，代理必须使用 `proxy_set_header X-Real-IP $remote_addr` 覆盖客户端头。缺失/非法单值头回退为代理 peer，没有 socket context 的测试/嵌入调用共享 `unknown` 桶。限流最多保存 16,384 个键，每次固定最多检查 16 个过期键；容量用尽拒绝新键，避免淘汰仍有效的耗尽桶后重置其额度。

## 数据和安全边界

- 用户密码只保存 Argon2id 哈希。
- access/refresh token、邮箱验证 token、密码重置 token 只保存 SHA-256 哈希；平台客户端把 bearer token 放在 Keychain 或 Windows DPAPI 中。
- `account_devices` 只是应用登录设备登记，不是远程连接权限表。
- 客户端 `deviceId` 是安装标签，不是所有权凭据；设备登记键为 `(user_id, id)`。不同账号可使用同一安装标签，各自的登录/撤销/设备名称相互隔离，正常退出后换号保留原 ID。v4 迁移保留旧 session 的 token/family/rotation 历史，为旧全局改绑留下的跨账号历史补已撤销的通用占位设备，不复制另一账号设备名称，也不复活旧会话；坏源或迁移失败原子回滚。
- 服务端不保存 PairingInvite 原文、relay authentication、远程主机私钥、控制端身份私钥或视频/输入数据。
- 账号登录不会复制其他设备的已保存主机；退出登录会撤销当前应用会话和设备登记，但不会代替主机端撤销可信控制端。
- SQLite 仍是单写者本地存储；租约支持进程重启和本地连接竞争，不代表跨主机共享 SQLite 或水平扩展。

## 历史保留

worker 每分钟最多执行一轮回收，各表独立短事务、默认每表最多删除 500 行：终态邮件保留 30 天，过期 action token 再保留 7 天，审计保留 90 天。时间恰好等于边界的记录保留；所有 outbox 引用的 token 都保留，避免外键级联删除待发任务。整个刷新令牌 family 要到最后一个成员到期后再过 30 天才可回收，仍活跃的 family 会保留最旧的 rotated token，使旧令牌重放仍能撤销后代。

按时间与引用索引清理，删除数量有界；受保护 family/token 可能需要跳过较多候选，因此不承诺所有历史分布下的锁时长硬上界。待发积压和长期活跃 family 有意保留，数据库增长应结合入队速率、worker 吞吐、最长 family 生命周期及各保留期解释。删除腾出的页供 SQLite 复用，不立刻归还 OS；磁盘告警、备份和需要时的离线 VACUUM 由运维另行安排，不在用户请求中做全库压缩。

## 备份与恢复

停止服务或使用 SQLite 在线备份工具复制数据库，再备份到加密且访问受控的存储。恢复后先运行 `/ready`，再执行一次注册、验证、登录和找回密码的受控冒烟测试。不要把 SQLite 文件、邮件 token 或服务环境变量提交到 Git。
