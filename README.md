# DeskLink

DeskLink 是面向个人设备的远程桌面工具。当前产品范围集中在 Windows 10/11 x64：同一个桌面应用同时承担被控制端和控制端，两台电脑可以通过受 TLS 保护的 QUIC 中继建立 Noise 端到端加密会话。

macOS 与 iOS 源码仍保留在仓库中，但不属于当前 Windows 发布门禁，也不会阻塞 Windows 构建。

## 当前状态

Windows 端已经接通：

- DXGI 主显示器采集与 Media Foundation H.264 实时编码；
- QUIC 中继、Noise 双向身份认证、独立视频/光标/输入安全通道；
- Windows DPAPI 保护的设备身份、主机连接、已保存控制端连接与可信设备列表；
- 一次性连接码、主机原生批准、可信重连与明确撤销；
- Tauri 2 + TypeScript 中文桌面界面、单实例、托盘和登录启动；
- 鼠标、滚轮、常用键、导航键、F1–F12、组合键与批量中文文字发送；
- 中继故障后的分轮有界、总体持续重连，以及 QUIC、Noise、采集、编码和输入边界的完整重建；
- 当前用户、无需管理员权限的单文件安装器，以及 Authenticode 签名构建链。

默认首次运行只需点击“启用远程连接”。Rust 会生成随机会话凭据并用当前 Windows 用户的 DPAPI 保存，应用使用已部署的公网中继 `101.35.246.159:4433` / `turn.p2p.yxswy.com`。自建中继只保留为高级设置。旧版 `127.0.0.1` / `localhost` 主机与控制端配置会在启动时自动迁移。

当前明确不支持：多显示器选择/虚拟桌面合成、远程音频、剪贴板同步、文件传输、自动更新、UAC 安全桌面和 `Ctrl+Alt+Delete` 注入。

## Windows 架构

- `apps/windows-ui`：唯一发布桌面入口。WebView2 只负责界面、视频解码与受限输入采集；Tauri capability 不开放 shell 或任意文件系统。
- `apps/windows`：Windows DXGI、Media Foundation、`SendInput`、DPAPI、原生批准框、诊断和 host runtime。
- `crates/desklink-protocol`：有界协议 DTO、序列化与输入/视频校验。
- `crates/desklink-crypto`：设备身份、配对邀请、Noise 会话与业务 AEAD。
- `crates/desklink-transport`：QUIC/TLS 客户端和 relay 加入协议。
- `crates/desklink-session`、`crates/desklink-video`：会话恢复策略与视频分片/重组。
- `server/relay`：只负责会话匹配、限流和密文转发，不解码桌面或输入内容。
- `tools/windows-installer`：只封装已构建的 `DeskLink.exe`，升级时会清理旧版辅助 host。

详细审计结论见 [`docs/windows-architecture-review.md`](docs/windows-architecture-review.md)。

## 开发与验证

推荐在 Windows x64 开发机执行：

```powershell
cd apps/windows-ui
bun install --frozen-lockfile
bun test
bun run build
cd ../..

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
python scripts/verify-windows-release.py
```

`verify-windows-release.py` 会检查版本一致性、冻结依赖安装、前端测试/构建、生产资源中是否残留 localhost、启用 `custom-protocol` 构建 x64 PE，并输出 `dist/windows/windows-release-verification.json`。GitHub Windows CI 执行相同门禁。

正式中继可以从任意开发机做一次与产品相同的系统证书链和 QUIC 握手检查：

```powershell
python scripts/verify-managed-relay.py
```

结果写入 `dist/windows/managed-relay-verification.json`。仓库另有每半小时执行一次的 GitHub Actions 中继监控；它负责尽早发现入口、TLS 或 QUIC 故障，但不能替代第二节点、容量告警和外部通知。

真实硬件恢复验收：

```powershell
python scripts/verify-windows-resilience.py --soak-seconds 60
```

它会验证真实 DXGI 帧、H.264 编码、连续 relay 故障恢复、Windows suspend/resume callback 和本机加密媒体/光标持续传输。单机自动化不等同于第二台物理电脑上的网络与实际睡眠验收。

## 两台 Windows 电脑使用

两台电脑安装同一个 DeskLink 安装包。被控制电脑点击“启用远程连接”并创建连接码；控制电脑在“控制另一台”中粘贴完整连接码、检测中继、发起安全连接，再由主机核对完整身份并批准。

完整步骤、输入范围与故障处理见 [`docs/windows-two-pc-setup.md`](docs/windows-two-pc-setup.md)。

## Windows 安装包

```powershell
python scripts/build-windows-installer.py
```

输出为 `dist/windows/DeskLinkSetup-0.1.1-x64.exe`。安装器是单文件、当前用户、无需管理员权限的 GUI 安装包，只部署现代入口 `DeskLink.exe` 到 `%LOCALAPPDATA%\Programs\DeskLink`，并创建开始菜单、Apps & Features 与可选登录启动项。安装器构建会先执行完整 Windows 发布验证，再直接封装同一份已验证程序；最后确认安装器包含与清单哈希完全一致的负载，并写入 `dist/windows/windows-installer-manifest.json`。

静默安装使用 `--quiet`，同时指定 `--no-autostart` 可关闭登录启动。默认卸载保留 `%LOCALAPPDATA%\DeskLink` 下的身份、连接、信任和诊断数据；只有显式 `--remove-data` 才删除用户数据。

未设置签名环境变量时，构建结果会明确标记为 `unsigned`，只适合本地测试。正式分发必须先签 `DeskLink.exe`，再封装并签最终安装器；配置见 [`docs/windows-code-signing.md`](docs/windows-code-signing.md)。代码签名与 SmartScreen 信誉是发布要求，不代表 Tauri 或原生 Windows 兼容性优劣。

## 安全与本地数据

当前 Windows 用户的数据位于 Known Folder API 返回的 Local AppData 下：

- `identity.bin`：Ed25519 设备身份；
- `connection.bin`：主机中继/会话设置；
- `controller-connection.bin`：已保存控制端连接；
- `trusted-controllers.bin`：已批准控制端；
- `logs/host.jsonl`：限额轮转且脱敏的诊断事件。

敏感二进制存储均由 Windows DPAPI 保护并原子替换。连接码、relay 加入密钥、私钥和完整设备身份不会写入诊断日志。配对批准与撤销继续使用默认拒绝的 Win32 原生确认窗口，不降级为 WebView 对话框。

## 自建中继与开发入口

生产 relay 需要 Windows 信任的 TLS 证书链，并开放 UDP 监听端口。主要配置包括 `DESKLINK_RELAY_ADDR`、`DESKLINK_RELAY_CERT_PEM`、`DESKLINK_RELAY_KEY_PEM` 和可选的 `DESKLINK_RELAY_SESSION_TTL_S`。没有 PEM 时生成的 `localhost` 自签名证书只用于自动化测试。

`desklink-windows` 命令行程序仍保留为开发与诊断工具，但不再嵌入安装器。现代 `DeskLink.exe` 是唯一用户入口。

## 发布前仍需人工验收

1. 两台物理 Windows 电脑连续运行数小时，覆盖断网、Wi-Fi 切换和实际睡眠/唤醒；
2. 使用正式代码签名身份构建，在干净 Windows 10/11 用户上验证 SmartScreen、安装、升级、登录启动与卸载；
3. 对公网 relay 做容量、告警、证书续期和故障切换演练；
4. 根据真实用户需求决定多显示器、剪贴板、文件传输、音频和自动更新的优先级。
