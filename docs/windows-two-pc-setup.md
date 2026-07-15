# DeskLink 两台 Windows 电脑部署

## 可用范围

同一个 `DeskLinkSetup-0.1.1-x64.exe` 同时包含 host 和 controller。两台 Windows 10/11 x64 电脑安装后，可以由 A 控制 B，也可以重新配对后由 B 控制 A。当前版本捕获 host 的 Windows 主显示器；多显示器选择与整块虚拟桌面尚未提供。

## 1. 选择连接方式

### 同一局域网（推荐）

`0.1.1` 起，DeskLink 会在主机电脑上自动启动内置 UDP 中继，不需要另外部署服务器。主机保留默认的 `127.0.0.1:4433` 和 `localhost`，保存连接后创建配对连接码即可。连接码会携带主机的局域网地址，控制端完整粘贴后会自动填写，无需手工抄写 IP。主机“概览”会列出当前可用网卡，并优先推荐物理 Wi-Fi 或以太网；如果电脑装有 WSL、VPN 或虚拟机，创建连接码时可以切换到与另一台电脑处于同一网络的地址。

两台电脑需要连接到同一路由器或可互通的局域网。Windows 防火墙首次询问时，请允许 DeskLink 访问“专用网络”；不要为公共网络放行。如果路由器启用了访客网络隔离、无线客户端隔离或两台电脑处在互不相通的 VLAN，仍然需要使用下面的公网中继。

### 跨网络或公网中继

DeskLink 使用 UDP QUIC relay。两台电脑都必须能访问 relay 的 IP 和 UDP 端口（默认 `4433`）。relay 只转发经过 Noise 会话加密的业务数据，但 QUIC 入口仍必须使用 Windows 信任的 TLS 证书。

生产 relay 需要设置：

- `DESKLINK_RELAY_ADDR`：监听地址，例如 `0.0.0.0:4433`；
- `DESKLINK_RELAY_CERT_PEM`：包含完整证书链的 PEM 文件；
- `DESKLINK_RELAY_KEY_PEM`：对应的 PKCS#1、PKCS#8 或 SEC1 PEM 私钥；
- `DESKLINK_RELAY_SESSION_TTL_S`：可选，默认 `86400` 秒，可设置范围为 60 秒到 30 天。

示例：

```sh
DESKLINK_RELAY_ADDR=0.0.0.0:4433 \
DESKLINK_RELAY_CERT_PEM=/etc/letsencrypt/live/relay.example.com/fullchain.pem \
DESKLINK_RELAY_KEY_PEM=/etc/letsencrypt/live/relay.example.com/privkey.pem \
DESKLINK_RELAY_SESSION_TTL_S=86400 \
cargo run -p desklink-relay --release
```

允许 relay 主机防火墙和云安全组的 UDP `4433` 入站。证书中的 DNS 名称（例如 `relay.example.com`）必须与 DeskLink 中填写的 `TLS server name` 完全一致。桌面端的 `Relay address` 当前要求填写可解析后的 IP 与端口，例如 `203.0.113.10:4433`。

未提供 PEM 配置时 relay 会生成 `localhost` 自签名证书，此模式只供自动测试和本机开发，不能直接用于两台普通 Windows 电脑。

## 2. 配置 host 电脑

1. 安装并打开 DeskLink，进入“连接设置”。同一局域网使用默认值；跨网络时填写公网 relay 的 `IP:端口` 和证书 DNS 名称。
2. 点击“生成安全凭据”，再点击“保存连接”。会话 ID 和中继密钥由 WebView2 的系统加密随机数生成器创建，并由 Rust 使用当前 Windows 用户的 DPAPI 保存。
3. 回到“概览”，确认“局域网连接检查”显示“可连接”。如果列出多个网卡，记住与另一台电脑处于同一网络的地址。
4. 进入“可信设备”，点击“配对设备”。按需选择局域网地址，再复制完整的多行配对连接码。
5. 只通过自己的可信渠道把连接码交给控制端电脑。连接码包含批准后用于重连的中继加入凭据，不应保存到公开聊天、工单或日志。

## 3. 连接 controller 电脑

1. 安装并打开同一个 DeskLink，进入“控制电脑”。
2. 完整粘贴主机生成的配对连接码；局域网地址和 TLS 名称会自动填写。
3. 先点击“检测连接”。检测通过表示控制端已完成到中继的 QUIC 握手；检测失败时按界面提示检查主机、专用网络防火墙和访客 Wi-Fi 隔离。
4. 点击“安全连接”。
5. 回到主机，核对 Windows 原生确认框中的完整设备 ID 和公钥指纹，然后明确批准。
6. 控制端收到视频配置后才会用 DPAPI 保存这台主机的连接材料。以后可在“已保存的电脑”中一键“重新连接”。

控制画面获得焦点后，鼠标、滚轮、字符键、Enter、Escape、Backspace、Tab、方向键及 Shift/Control/Alt/Meta 组合会发送到 host。Windows 的安全桌面组合键 `Ctrl+Alt+Delete` 不能由普通 `SendInput` 注入，需要在 host 本地操作。

## 4. 反向控制

如果还需要 B 控制 A，在 A 上配置 host connection 并创建新的邀请，在 B 上重复配对。每个 Windows 用户都有独立的设备身份、可信列表、host connection 和 saved controller connection。

## 故障检查

- 主机显示“没有可用的局域网地址”：先连接 Wi-Fi 或网线，再刷新状态并重新创建连接码。
- 主机显示“局域网中继启动失败”：UDP `4433` 可能已被其他程序占用，关闭占用程序或重启 DeskLink 后再试。
- 控制端“检测连接”失败：确认两台电脑处于同一局域网、主机 DeskLink 正在运行、Windows 防火墙已允许专用网络；访客 Wi-Fi、AP 隔离和互不相通的 VLAN 也会阻止连接。
- 一直显示“正在连接中继服务器”：确认 UDP 端口可达、中继正在监听、地址使用 IP 而不是 DNS。
- TLS 立即失败或不断重连：确认 `TLS server name` 与证书 SAN 一致，证书链受两台 Windows 信任，并且 relay 已加载完整链。
- 一直显示“请在主机上批准此电脑”：检查主机是否显示原生审批框，或该邀请是否已经过期。
- 已批准但没有画面：更新 Microsoft Edge WebView2 Runtime；controller 页面会在缺少 H.264 WebCodecs 支持时给出明确错误。
- 重连失败：主机必须仍使用创建邀请时的会话 ID 和中继密钥；如果在“连接设置”中重新生成了凭据，需要重新配对。
- Windows 下载提示“不安全”：这属于 Authenticode/SmartScreen 发布信誉，不是连接兼容性问题。正式分发仍需按 `docs/windows-code-signing.md` 对应用、辅助 host 和安装器签名。
