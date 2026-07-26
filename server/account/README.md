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

请求使用 `Authorization: Bearer <DESKLINK_ACCOUNT_MAIL_TOKEN>`。如果 provider 暂时失败，注册或找回密码请求仍然返回统一成功结果，邮件任务进入 SQLite outbox，由进程每 30 秒重试，最多 10 次。outbox 只保存邮件模板和 action token 记录 ID，原始链接 token 只在单次投递的内存中生成，不写入数据库。

## HTTP 检查

- `GET /health`：进程存活检查，返回 `status: ok`。
- `GET /ready`：数据库迁移已加载并可以接受请求时返回 `status: ready`。

生产环境只应通过反向代理暴露 HTTPS。反向代理需要正确传递 `X-Forwarded-For`，并且只能由可信代理写入该头；服务端会使用它进行账号接口限流。

## 数据和安全边界

- 用户密码只保存 Argon2id 哈希。
- access/refresh token、邮箱验证 token、密码重置 token 只保存 SHA-256 哈希；平台客户端把 bearer token 放在 Keychain 或 Windows DPAPI 中。
- `account_devices` 只是应用登录设备登记，不是远程连接权限表。
- 服务端不保存 PairingInvite 原文、relay authentication、远程主机私钥、控制端身份私钥或视频/输入数据。
- 账号登录不会复制其他设备的已保存主机；退出登录会撤销当前应用会话和设备登记，但不会代替主机端撤销可信控制端。
- 当前 outbox 设计按单个 Bun 进程运行；不要让多个服务实例同时写同一 SQLite 文件。需要水平扩展时应先迁移到 PostgreSQL 和带租约的队列。

## 备份与恢复

停止服务或使用 SQLite 在线备份工具复制数据库，再备份到加密且访问受控的存储。恢复后先运行 `/ready`，再执行一次注册、验证、登录和找回密码的受控冒烟测试。不要把 SQLite 文件、邮件 token 或服务环境变量提交到 Git。
