# DeskLink

DeskLink 是面向个人设备的端到端加密远程桌面工具。当前仓库同时包含 Windows 10/11 x64 桌面端与 macOS Apple Silicon 桌面端；两端共享 Rust 协议、密码学、QUIC 传输、会话、视频和 C ABI 核心，中继只负责匹配会话与转发业务密文。

## 当前状态

### Windows 10/11 x64

- 同一个 Tauri 2 + TypeScript 中文桌面应用同时承担被控制端和控制端；
- DXGI 多显示器采集、带输入清理和超时恢复的屏幕切换、不同排列下的物理像素坐标映射、适应窗口与 1:1 画面缩放、1:1 本地拖动浏览、保留控制栏的全屏模式、Media Foundation H.264 实时编码与带迟滞的自适应画质；
- QUIC/TLS 中继、Noise 双向身份认证，以及独立视频、光标和输入安全通道；
- Windows DPAPI 保护的设备身份、连接设置、已保存控制端连接与可信设备列表；
- 稳定设备 ID、临时/固定访问密码、主机原生批准、可信重连与明确撤销；
- 鼠标、滚轮、常用键、导航键、F1–F12、独立 Ctrl/Alt/Shift/Windows 修饰键、组合键与批量中文文字发送；每个实时输入绑定当前视频流，断线重连不会重放旧会话输入；
- 双向纯文本剪贴板、经远端确认的双向文件传输、多文件队列、实时速度与失败后的显式重新发送；
- 被控端 Windows 系统输出声音、64 kbit/s Opus 压缩、单包丢失恢复、控制端低延迟播放，以及工具栏即时静音；
- 中继故障后的持续重连，以及 QUIC、Noise、采集、编码和输入边界完整重建；
- 用户主动开启的脱敏云端诊断、会话关联和断网自动补传；
- 当前用户、无需管理员权限的单文件安装器，以及 Authenticode 签名构建链。

默认首次运行只需点击“启用远程连接”。Rust 会生成随机会话凭据并用当前 Windows 用户的 DPAPI 保存，应用使用公网中继 `101.35.246.159:4433` / `turn.p2p.yxswy.com`。自建中继只保留在高级设置中，普通用户路径不包含旧版连接码或本地 loopback 中继。

### macOS Apple Silicon

- SwiftUI host/controller 角色界面与稳定的 Rust C ABI；
- Screen Recording 与 Accessibility 权限检查；
- 屏幕采集、H.264 编码、VideoToolbox 解码和 Metal 等比显示；
- 鼠标、滚轮、键盘输入映射，以及断开时的可靠输入释放；
- 一次性邀请、本机审批、可信控制端撤销与已批准主机重连；
- Keychain 保护的长期身份、可信控制端和已批准主机材料；
- 原生 arm64 `.app` 构建、架构检查、Swift 测试与 Rust FFI 运行时验证。

macOS 源码和验证链已经合入主仓库，但 Apple 平台构建仍必须在 Apple Silicon Mac 上执行，不阻塞 Windows 发布门禁。

当前明确不支持：麦克风/语音对讲、整块虚拟桌面合成、自动更新，以及 Windows UAC 安全桌面和 `Ctrl+Alt+Delete` 注入。多显示器通过会话内切换当前屏幕实现；剪贴板采用用户主动触发的纯文本传输，不在后台自动监听。

## 架构

- `crates/desklink-protocol`：有界协议 DTO、序列化与输入/视频校验；
- `crates/desklink-crypto`：设备身份、配对邀请、Noise 会话与业务 AEAD；
- `crates/desklink-transport`：QUIC/TLS 客户端和 relay 加入协议；
- `crates/desklink-session`、`crates/desklink-video`：会话恢复与视频分片/重组；
- `crates/desklink-ffi`：供 macOS 使用的稳定 controller/host C ABI 与可取消后台 worker；
- `apps/windows-ui`：Windows 唯一发布入口，WebView2 负责界面、视频解码与受限输入采集；
- `apps/windows`：DXGI、Media Foundation、`SendInput`、DPAPI、原生批准、诊断和 host runtime；
- `apps/macos`：SwiftUI、ScreenCapture、VideoToolbox、Metal、系统权限与 Keychain 适配；
- `server/relay`：会话匹配、限流和密文转发，不解码桌面或输入；
- `tools/windows-installer`：只封装已验证的 `DeskLink.exe`，升级时清理旧版辅助 host。

Windows 详细审计见 [`docs/windows-architecture-review.md`](docs/windows-architecture-review.md)。macOS 设计和实施记录见 [`docs/superpowers/specs/2026-07-16-macos-desktop-completion-design.md`](docs/superpowers/specs/2026-07-16-macos-desktop-completion-design.md) 与 [`docs/superpowers/plans/2026-07-16-macos-desktop-completion.md`](docs/superpowers/plans/2026-07-16-macos-desktop-completion.md)。

## 共享 Rust 验证

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Windows 开发与发布验证

在 Windows x64 开发机执行：

```powershell
cd apps/windows-ui
bun install --frozen-lockfile
bun test
bun run build
cd ../..

python scripts/verify-windows-release.py
python scripts/verify-windows-resilience.py --soak-seconds 60
python scripts/verify-managed-relay.py
```

`verify-windows-release.py` 会检查版本一致性、前后端正式中继配置、冻结依赖安装、前端测试与构建、全部生产资源中的开发地址、`custom-protocol` x64 PE、哈希和大小，并输出 `dist/windows/windows-release-verification.json`。

应用图标作为版本化资源提交到 `apps/windows/assets`；`scripts/generate-windows-assets.py` 只用于设计资源发生变化时重新生成，发布门禁会直接验证 PNG/ICO 文件签名与哈希，不依赖 CI 临时安装图像处理库。

`verify-windows-resilience.py` 会验证真实 DXGI 帧、H.264 编码、连续 relay 故障恢复、Windows suspend/resume callback，以及本机加密媒体和光标持续传输。它不等同于第二台物理电脑上的实际网络与睡眠验收。

`verify-managed-relay.py` 使用产品 transport 客户端执行系统证书链和 QUIC 握手，结果写入 `dist/windows/managed-relay-verification.json`。GitHub Actions 另有定时外部握手监控。脱敏诊断的隐私边界、部署和排查方式见 [`docs/cloud-diagnostics.md`](docs/cloud-diagnostics.md)。

## 两台 Windows 电脑使用

两台电脑安装同一个 DeskLink 安装包。被控制电脑启用远程连接并生成临时密码或固定密码；控制电脑输入设备 ID 和访问密码后发起安全连接，再由主机核对完整身份并批准。连接后可在工具栏选择手动画质或“自动”画质，并在“适应”和“1:1”之间切换远程画面；1:1 下的“浏览画面”只移动本地视野，不会误操作远端，全屏时工具栏仍然可用。切换远端显示器会先释放仍按下的输入，失败或超时不会破坏当前会话。“Windows 键”可直接打开远程开始菜单，Ctrl/Shift 与鼠标组合操作会保持到实际松开。也可在“传输”面板中双向复制纯文本，或选择文件发送到被控电脑的“下载”文件夹，文件发送前始终需要被控端确认。

完整步骤、输入范围与故障处理见 [`docs/windows-two-pc-setup.md`](docs/windows-two-pc-setup.md)。

## Windows 安装包

```powershell
python scripts/build-windows-installer.py
```

输出为 `dist/windows/DeskLinkSetup-<version>-x64.exe`。构建脚本会先执行完整 Windows 发布验证，再原样封装同一份已验证程序，确认最终安装器包含与清单哈希完全一致的负载，并写入 `dist/windows/windows-installer-manifest.json`。

安装器是单文件、当前用户、无需管理员权限的 GUI 安装包，只部署 `DeskLink.exe` 到 `%LOCALAPPDATA%\Programs\DeskLink`。静默安装使用 `--quiet`，同时指定 `--no-autostart` 可关闭登录启动。默认卸载保留 `%LOCALAPPDATA%\DeskLink` 下的身份、连接、信任和诊断数据；只有显式 `--remove-data` 才删除用户数据。

未设置签名环境变量时，构建结果会标记为 `unsigned`，只适合本地测试。正式分发必须先签应用、再封装并签最终安装器；配置见 [`docs/windows-code-signing.md`](docs/windows-code-signing.md)。

正式发布必须使用强制签名门禁：

```powershell
python scripts/build-windows-installer.py --require-signing
```

远程的 `Windows Signed Release` 工作流只接受 GitHub Secrets 中的可信 PFX，缺少证书、证书用途不正确、证书过期、签名失败或没有 RFC 3161 时间戳都会中止，不会上传未签名安装器。

## macOS 构建与使用

在 Apple Silicon Mac 上执行：

```sh
cargo test --workspace
cd apps/macos && swift test --arch arm64
cd ../..
./scripts/build-macos-arm64.sh --check
./scripts/verify-macos-runtime.sh
open dist/macos/DeskLink.app
```

`build-macos-arm64.sh --check` 会先构建 `aarch64-apple-darwin` Rust FFI，再构建 arm64 Swift release executable，生成 `dist/macos/DeskLink.app`，并检查可执行文件架构、bundle identifier、Screen Recording 声明和最低系统版本。`verify-macos-runtime.sh` 运行 Rust FFI、local-relay fake-media 端到端测试与 Swift arm64 测试。

macOS 主窗口与 Windows 使用同一套 remote-task-first 中文页面结构：`连接设备`、`最近设备`、`共享此设备`、`已批准设备` 和 `设置 / 诊断`。控制端在“连接设备”粘贴连接码或选择最近设备；主机在“共享此设备”授予屏幕录制与辅助功能权限、创建连接码，并在本机核对控制端身份后批准。

当前仓库没有 `apps/ios` 客户端工程，因此 iOS 本轮只有统一页面契约，没有可执行产物。iOS 默认进入“连接设备”，可使用底部导航或 sheet；“共享此设备”不得渲染成可执行的 iOS 被控入口。

长期身份、可信控制端和已批准 host 的重连材料保存在当前用户 Apple Keychain。邀请仅应通过可信渠道传递；身份更换、撤销、邀请过期或凭据变化后，应重新创建邀请并配对。

macOS 开发联调环境变量包括 `DESKLINK_RELAY_URL` 与 `DESKLINK_RELAY_SERVER_NAME`；正常签名邀请会携带连接所需的受保护材料，不应通过命令行或日志传递密钥。

## 安全与本地数据

Windows 当前用户数据位于 Known Folder API 返回的 Local AppData 下：

- `identity.bin`：Ed25519 设备身份；
- `connection.bin`：主机中继和会话设置；
- `controller-connection.bin`：已保存控制端连接；
- `trusted-controllers.bin`：已批准控制端；
- `logs/host.jsonl`：限额轮转且脱敏的诊断事件。
- `logs/controller.jsonl`：控制端连接阶段、重试与画面计数的脱敏事件；
- `diagnostics-sharing.enabled`：用户主动开启诊断共享后写入的非敏感开关标记。

敏感 Windows 二进制存储均由 DPAPI 保护并原子替换。macOS 敏感身份与连接材料使用 Keychain。连接码、relay 加入密钥、私钥和完整设备身份不会写入诊断日志；配对批准与撤销保持系统原生、默认拒绝的确认边界。

## 自建中继

生产 relay 需要受客户端系统信任的 TLS 证书链，并开放 UDP 监听端口。主要配置包括 `DESKLINK_RELAY_ADDR`、`DESKLINK_RELAY_CERT_PEM`、`DESKLINK_RELAY_KEY_PEM`、`DESKLINK_RELAY_SESSION_TTL_S` 和容量限制变量。没有 PEM 时生成的 `localhost` 自签名证书只用于自动化测试。

当前加入中继信封会携带非秘密的稳定设备参与者 ID 和目录注册信息：同一设备重连时可原子替换自己的旧连接，不同控制端仍受单控制端限制。涉及加入协议升级时，必须先升级 relay，再发布使用新协议的客户端。

## 发布前仍需人工验收

1. 两台物理 Windows 电脑连续运行数小时，覆盖跨网、断网、Wi-Fi 切换和实际睡眠/唤醒；
2. 使用正式代码签名身份，在干净 Windows 10/11 上验证 SmartScreen、安装、升级、登录启动与卸载；
3. 两台 Apple Silicon Mac 完成 Screen Recording/Accessibility 授权、首次邀请配对、输入和断网恢复验收；
4. 为公网 relay 增加第二节点、容量与证书告警，并执行故障切换演练；
5. 完成正式自动更新与签名发布链，并继续验证多显示器在不同 DPI、缩放模式与排列下的坐标一致性。
