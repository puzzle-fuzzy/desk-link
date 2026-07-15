# DeskLink 腾讯云中继部署

这套部署用于当前腾讯云主机 `101.35.246.159`，与 `/opt/p2p-transmission` 完全分离。它不会启动第二个 API，不会读写 SQLite，也不会重启现有 Web、API 或 coturn 容器。

## 生产连接参数

- DeskLink 中继地址：`101.35.246.159:4433`
- TLS 服务器名称：`turn.p2p.yxswy.com`
- 公网协议：仅 UDP `4433`
- 容器目录：`/opt/desklink-relay`
- 证书来源：只读挂载 `/opt/p2p-transmission/deploy/coturn/.local/tls`

DeskLink 复用现有 `turn.p2p.yxswy.com` 证书，但使用独立 UDP 端口。不要把 coturn 的 TURN secret、p2p API `.env` 或 SQLite 数据复制进 DeskLink 目录。

## 1. 腾讯云和主机防火墙

在腾讯云安全组增加一条 UDP `4433` 入站规则。远程设备网络不固定时来源使用 `0.0.0.0/0`；不要同时开放 TCP `4433`，也不要更改现有 TURN 端口范围。

如果主机启用了 UFW，再以 root 执行：

```sh
ufw allow 4433/udp comment 'DeskLink QUIC relay'
ufw status numbered
```

## 2. 在 Windows 构建 Linux 镜像

需要正在运行 Linux 容器的 Docker Desktop：

```sh
python scripts/build-linux-relay-image.py
```

输出位于 `dist/linux/desklink-relay-0.1.0-amd64.tar`。构建脚本会固定 `linux/amd64`，运行容器冒烟检查，并输出镜像与归档 SHA-256。

## 3. 上传和启动

确认 SSH host key 已经存在于本机 `known_hosts`，然后上传镜像和 Compose 文件：

```sh
scp dist/linux/desklink-relay-0.1.0-amd64.tar root@101.35.246.159:/tmp/
scp deploy/tencent-relay/compose.yml root@101.35.246.159:/tmp/desklink-relay-compose.yml
```

在服务器上执行：

```sh
set -eu
test -r /opt/p2p-transmission/deploy/coturn/.local/tls/fullchain.pem
test -r /opt/p2p-transmission/deploy/coturn/.local/tls/privkey.pem
install -d -m 0755 /opt/desklink-relay
install -m 0644 /tmp/desklink-relay-compose.yml /opt/desklink-relay/compose.yml
docker load -i /tmp/desklink-relay-0.1.0-amd64.tar
docker compose -f /opt/desklink-relay/compose.yml config --quiet
docker compose -f /opt/desklink-relay/compose.yml up -d
docker compose -f /opt/desklink-relay/compose.yml ps
docker compose -f /opt/desklink-relay/compose.yml logs --tail=50 relay
ss -lunp | grep ':4433'
```

容器必须显示 `healthy`，日志应包含 `DeskLink relay listening on 0.0.0.0:4433`。健康检查会重新读取地址、证书、私钥和会话有效期配置，但不会占用第二个 UDP 端口。现有 `p2p-transmission` 容器状态应保持不变。

## 4. 两台 Windows 电脑验收

在作为主机的 DeskLink 中进入“本机连接”，填写：

- 中继地址：`101.35.246.159:4433`
- TLS 服务器名称：`turn.p2p.yxswy.com`

保存后创建连接码。在另一台电脑的“控制另一台”页面粘贴连接码，先点击“先检测网络”，检测通过后再点击“连接并请求批准”。回到主机核对身份并批准，然后验证画面、鼠标、键盘、断网重连和睡眠恢复。

UDP 无法用普通 HTTP 健康检查代替。服务器 `healthy` 只证明进程和证书可读，最终必须以 Windows 客户端完成 QUIC/TLS 握手。

## 5. 更新、证书续期和回滚

更新只需要加载新镜像并重新执行 `docker compose up -d`。证书文件更新后必须重启 DeskLink 中继，Rustls 才会重新加载证书：

```sh
docker compose -f /opt/desklink-relay/compose.yml restart relay
```

需要立即停用 DeskLink 时，仅停止自己的 Compose 项目：

```sh
docker compose -f /opt/desklink-relay/compose.yml stop relay
```

不要执行 `docker compose down` 操作 `/opt/p2p-transmission/deploy/compose.yml`，不要删除现有证书、`.env` 或 `deploy/data`。
