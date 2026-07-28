# DeskLink 账号服务发布手册

## 发布前

1. 在服务端创建独立的生产 SQLite 持久目录，并确认运行用户可写。
2. 准备 HTTPS 域名，例如 `account.example.com`，证书由反向代理管理。
3. 配置以下变量：

```sh
NODE_ENV=production
DESKLINK_ACCOUNT_ADDR=0.0.0.0
DESKLINK_ACCOUNT_PORT=3412
DESKLINK_ACCOUNT_DATABASE=/data/desklink-account.sqlite
DESKLINK_ACCOUNT_ORIGIN=https://account.example.com
DESKLINK_ACCOUNT_CORS_ORIGINS=
DESKLINK_ACCOUNT_MAIL_URL=https://mail-provider.example/v1/send
DESKLINK_ACCOUNT_MAIL_TOKEN=replace-on-server-only
DESKLINK_ACCOUNT_MAIL_FROM='DeskLink <noreply@example.com>'
```

4. 确认客户端的账号 URL 已指向相同域名：
   - macOS/iOS 可通过 `DeskLinkAccountURL` 配置覆盖默认值；
   - Windows 使用 `DESKLINK_ACCOUNT_URL`。
5. 确认 DNS、TLS、邮件 SPF/DKIM/DMARC 和 provider 配额已经就绪。

## 容器启动

```sh
docker build -t desklink-account:local server/account
docker run --rm \
  --name desklink-account \
  --env-file ./desklink-account.env \
  -p 127.0.0.1:3412:3412 \
  -v /opt/desklink-account/data:/data \
  desklink-account:local
```

生产环境应由 systemd、Docker Compose 或同等 supervisor 托管，不要直接把 Bun 端口暴露到公网。

## 反向代理要求

- 只允许 HTTPS；HTTP 只做明确的 301 到 HTTPS 或直接拒绝。
- 将 `/health` 用于存活探针，`/ready` 用于就绪探针。
- 将客户端真实 IP 通过可信 `X-Forwarded-For` 传递；公网用户不能直接覆盖该头到应用。
- 限制请求体大小和上游超时；账号服务本身也会校验 JSON 结构、邮箱长度、密码长度和 token 长度。
- 不缓存 `/v1/account/*` 响应，尤其是验证和重置页面。

## 冒烟验证

```sh
DESKLINK_ACCOUNT_URL=https://account.example.com \
  ./scripts/verify-account-service.sh
```

该脚本只验证 `/health` 和 `/ready`，不会创建真实账号。真实注册、邮箱验证、登录、refresh、退出和密码重置必须使用专门的测试邮箱手工验收，完成后删除测试账号或清空测试环境。

## 邮件失败与恢复

- 注册/找回密码的 HTTP 成功不代表邮件已经送达；服务会把消息写入 outbox 并在后台重试。
- 查看容器日志中的 outbox 错误摘要和 provider 状态；不要打印邮件正文中的 token。
- provider 恢复后无需重新注册，等待下一轮 flush 或重启服务触发一次 flush。
- 超过 10 次失败的消息不会继续自动重试，应修复 provider 后由运维脚本或数据库操作重新安排，操作前先备份。

## 回滚与恢复

保留上一版镜像和数据库备份。应用代码回滚前确认迁移是向后兼容的；不要直接删除 SQLite 数据库或 WAL 文件。恢复后先检查 `/ready`，再验证一个测试账号的登录和 refresh，最后恢复客户端流量。

账号能力是可选的；如果正式启用注册、登录和跨设备账号管理，必须先完成真实邮件 provider、域名、TLS、生产数据库备份和部署密钥配置。本仓库完成的是代码、接口和本地验证，不代表线上账号服务已经发布。未部署账号服务时，Windows 客户端仍应通过本机模式正常提供远程控制。
