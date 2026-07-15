# DeskLink

DeskLink 是面向个人设备的跨平台远程桌面工具，目标是在自己的 Windows、Mac 和 iPhone 之间稳定查看并控制电脑桌面。

当前优先级：

1. Windows 桌面端；
2. macOS Apple Silicon 桌面端；
3. iOS 控制端。

项目设计资料位于 [`docs/`](docs/)，原始需求文档已原样归档。实现将采用 Rust 共享核心 + Windows 原生能力 + macOS 原生能力，iOS 在桌面端链路稳定后接入。

## 当前阶段

当前仓库已完成共享 Rust 核心、QUIC 中继、稳定 C ABI 和 macOS Apple Silicon 控制端骨架，并接通 Windows DXGI 采集、Media Foundation H.264 实时编码、VideoConfig、QUIC 视频发送、独立光标通道和可安全释放的 `SendInput` 边界。Windows runtime 已使用 DPAPI 持久保存 Ed25519 设备身份、可信控制端列表及连接配置，并在能力协商、采集和输入注入之前完成 Noise XX 双向认证及本地授权。首次运行会显示原生连接设置窗口；会话 ID、relay 加入密钥及网络参数经校验后写入当前用户保护的 `connection.bin`，relay 密钥不会回显，托盘菜单也可随时重新配置。一次性 `PairingInvite` 会分别携带 relay 加入密钥、host 身份和有效期；Noise 签名同时绑定设备 ID、公钥和握手 transcript。Windows host 现已提供真实本地审批对话框，完整展示设备 ID、公钥指纹、会话 ID 和过期时间，默认选择拒绝；批准后才会持久化信任。后续新 relay 会话可从可信列表重连；未知设备、设备 ID 换钥、邀请过期、拒绝及损坏的 DPAPI 存储均失败关闭。host 与 controller 现已共同使用共享的有界指数退避策略（250 ms 起步、8 s 封顶、最多 6 次）。Windows host 在 relay 故障后会完整重建 QUIC、Noise、DXGI、编码器和输入注入边界，并为恢复后的安全会话分配递增 `stream_id`；稳定协商会重置重试预算，认证/授权、协议及本地采集编码错误则立即失败关闭。Windows 原生常驻托盘和状态窗口现已接入这些生命周期事件，集中展示连接状态、重试原因与完整可信控制端身份；支持刷新、连接设置、默认拒绝的逐项撤销、窗口关闭后继续驻留以及明确退出。窗口采用 Segoe UI 层级、原生 DPI 缩放、键盘可达控件，并已在真实 Windows 桌面完成配置校验、空状态、长文案、错误状态、重试和最大化/还原验收。生命周期及可信设备操作会写入限额轮转的 JSON Lines 诊断日志；日志只记录允许的状态字段，命名凭据和连续长十六进制值会先脱敏，状态窗口也不再展示底层错误原文。可信列表损坏或暂时不可用时，主窗口继续运行并提供明确的内联错误和刷新入口。Windows 当前用户单文件安装器、正式应用图标、开机启动和安装/升级/卸载边界现已完成；程序文件与 `%LOCALAPPDATA%\DeskLink` 用户数据严格分离，默认卸载保留身份、信任、连接配置及诊断数据。发布构建链现可选择 Microsoft Artifact Signing 或证书存储中的公有代码签名证书，按“应用先签、安装器后签”的顺序执行 SHA-256/RFC 3161 签名并强制 Authenticode 验证。Rust FFI crate 的真实 QUIC/Noise `ControllerRuntime` 已进入可取消后台 worker，负责认证握手、加密能力协商、VideoConfig/光标解密、视频分片重组、关键帧恢复和加密输入发送。C ABI 可直接验证并解析固定 181 字节签名 `PairingInvite`，篡改、过期和长度错误均不会启动网络 worker；macOS 配置层会优先使用单个 `DESKLINK_PAIRING_INVITE`，旧的分字段开发入口仍保留。macOS Swift 端已接通 VideoConfig → Annex B SPS/PPS → VideoToolbox、Annex B access unit → AVCC、异步最新帧发布和 Metal 等比显示，并通过 Apple Keychain 持久保存控制端身份；Apple 原生编译在当前 Windows 开发机上保持后置。已确认的设计规格位于：

[`docs/superpowers/specs/2026-07-15-desklink-design.md`](docs/superpowers/specs/2026-07-15-desklink-design.md)

## 下一阶段（Windows 优先）

1. 完成 Windows 真实双机长时间运行、断网恢复和多显示器验收；
2. 取得发布代码签名身份，用正式证书生成签名包并在干净 Windows 虚拟机完成 SmartScreen、升级和卸载验收；
3. 在 Apple Silicon 环境完成可信主机列表、Swift 配对 UI、arm64 链接与打包验收。

## 已验证命令

```sh
cargo test --workspace
./scripts/verify.sh
cd apps/macos && swift test --arch arm64
cd ../.. && ./scripts/build-macos-arm64.sh --check
cargo check --manifest-path apps/windows/Cargo.toml --target x86_64-pc-windows-msvc
cargo build --manifest-path apps/windows/Cargo.toml --target x86_64-pc-windows-msvc
cargo test --manifest-path apps/windows/Cargo.toml --test capture_smoke -- --nocapture
cargo test --manifest-path apps/windows/Cargo.toml --test encoder_smoke -- --nocapture
cargo test --manifest-path apps/windows/Cargo.toml --test runtime_smoke -- --nocapture
cargo run -p desklink-windows --example tray_preview
cargo test --manifest-path tests/end-to-end/Cargo.toml
python scripts/build-windows-installer.py
python scripts/sign-windows-artifact.py --verify-only dist/windows/DeskLinkSetup-0.1.0-x64.exe
```

macOS arm64 的 Swift 输入映射、Rust FFI 链接和 VideoToolbox/Metal 编译检查已有通过记录。Windows MSVC 环境已通过真实桌面纹理 → NV12 → H.264 → VideoConfig/分片 → `QuicClient` → 本机 relay → `ControllerRuntime` 完整 access unit 的 runtime 冒烟，并验证 Noise 双向身份认证、分通道 AEAD、乱序数据报重放保护、配置前丢包后的关键帧恢复和独立光标 Datagram；relay 只接触业务密文及连接/通道元数据。独立双端集成测试还验证了加密滚轮输入和关键帧请求只能由 host 对应安全通道解密。DXGI 超时与 access-lost 使用稳定错误分类，runtime 会在 access-lost 后重建采集器并请求新 IDR。输入边界已覆盖精确桌面端点、水平/垂直滚轮、Shift/Control/Alt/Meta 组合键、Unicode 代理对、扩展方向键和部分注入失败后的安全释放；测试使用可注入后端，不会操作本机键鼠。

Windows 真实码流已经接入持续运行的 QUIC host loop，并完成端到端业务载荷加密。本机 relay 冒烟现已覆盖一次性邀请首次配对、本地批准、DPAPI 持久化，以及同一控制端在全新 relay 会话中的可信重连；真实 relay 重启测试还确认 host 能重新建立端到端安全会话、重新初始化采集编码并切换到新视频流。负向测试确认用户拒绝、邀请过期、设备 ID 换钥、relay 认证不匹配和第二控制端会在采集与输入初始化之前被阻断；host 占用会按预算退避，而本地安全错误不会被重试掩盖。Windows 可执行入口已接入一次性配对审批、常驻托盘、状态主窗口和可信设备撤销；生产窗口与 `tray_preview` 示例共用同一实现。runtime 仍使用测试 TLS 证书，首次配对材料目前通过安全带外渠道复制完整签名邀请。macOS 解码显示边界、安全 C ABI worker、Keychain 身份和 Swift 事件入口均已接线，并补充 Annex B/AVCC、显示几何、配置解析及 ABI 生命周期测试；macOS arm64 的 Swift/链接/打包和真实跨机验收延后到 Apple Silicon 环境统一执行。

## Windows 安装包

在 Windows x64 开发机运行 `python scripts/build-windows-installer.py`，会生成 `dist/windows/DeskLinkSetup-0.1.0-x64.exe`。安装器为单文件、当前用户、无需管理员权限的 GUI 安装包：程序安装到 `%LOCALAPPDATA%\Programs\DeskLink`，开始菜单快捷方式和 Apps & Features 注册会同步创建，默认在 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 写入仅含程序路径及 `--startup` 的启动项。尚未配置连接参数时，普通启动会打开连接设置窗口；登录启动会保持在托盘并显示未配置状态，可从托盘菜单选择 `Connection settings...`。

静默安装使用 `--quiet`，同时指定 `--no-autostart` 可关闭自动启动。默认卸载只删除程序、快捷方式和注册项，保留 `%LOCALAPPDATA%\DeskLink` 下的身份、连接配置、可信控制端和诊断数据；只有显式使用 `--remove-data` 才会删除这些数据。未设置签名环境变量时构建结果会明确标记为 `unsigned`；正式签名配置和命令见 [`docs/windows-code-signing.md`](docs/windows-code-signing.md)。

## Windows host 手动运行

首次配对可设置 `DESKLINK_PAIRING_MODE=1`。host 会生成最长 600 秒有效的一次性签名邀请，在本地终端输出控制端当前仍需使用的 session、relay 加入密钥、host 公钥及完整邀请；这些值都是临时秘密，不应通过不可信渠道传播。控制端完成 Noise 身份认证后，Windows 会显示默认拒绝的本地审批对话框，只有确认同一设备身份后才会写入可信列表。

普通连接/可信重连使用以下参数：

- `DESKLINK_SESSION_ID`：32 个十六进制字符；
- `DESKLINK_AUTH_KEY`：64 个十六进制字符；
- `DESKLINK_PEER_VERIFY_KEY`：可选的开发回退控制端 Ed25519 公钥，64 个十六进制字符；未设置时只接受 DPAPI 可信列表中的控制端；
- `DESKLINK_APPROVE_SESSION=1`：仅在启用上述开发回退时必需；回退身份不会自动写入可信列表；
- `DESKLINK_RELAY_ADDR`：可选，默认 `127.0.0.1:4433`；
- `DESKLINK_RELAY_SERVER_NAME`：可选，默认 `localhost`；
- `DESKLINK_STREAM_ID`：可选，默认 `1`。
- `DESKLINK_MANAGE_TRUST=1`：逐项显示可信控制端并询问是否撤销，处理后直接退出，不启动远程会话。

Windows 长期身份默认保存在 `%LOCALAPPDATA%\DeskLink\identity.bin`，连接配置保存在 `%LOCALAPPDATA%\DeskLink\connection.bin`，可信控制端保存在 `%LOCALAPPDATA%\DeskLink\trusted-controllers.bin`；三者均由当前 Windows 用户的 DPAPI 保护并采用原子替换写入。普通首次运行或显式执行 `desklink-windows.exe --configure` 可保存连接设置；修改现有配置时 relay 密钥留空会保留原密钥。结构化诊断日志保存在 `%LOCALAPPDATA%\DeskLink\logs\host.jsonl`，单文件上限 512 KiB，最多保留 3 个轮转历史文件。诊断日志不写入 relay 加入密钥、配对邀请或私钥材料。环境变量配置继续作为开发覆盖入口，不会替代或自动修改 DPAPI 配置。

## macOS controller 手动运行

当前安全连接入口优先读取以下签名邀请配置：

- `DESKLINK_PAIRING_INVITE`：Windows 配对模式输出的 362 个十六进制字符；FFI 会验证固定长度、Ed25519 签名和有效期，并从中读取 session、relay 加入密钥及 host 公钥；
- `DESKLINK_RELAY_URL`：可选，默认 `quic://127.0.0.1:4433`；
- `DESKLINK_RELAY_SERVER_NAME`：可选，默认 `localhost`。

未设置 `DESKLINK_PAIRING_INVITE` 时，仍可使用以下开发回退字段：

- `DESKLINK_SESSION_ID`：32 个十六进制字符；
- `DESKLINK_AUTH_KEY`：64 个十六进制字符；
- `DESKLINK_HOST_VERIFY_KEY`：Windows host 的 Ed25519 公钥，64 个十六进制字符。

macOS 控制端的长期设备 ID 和私钥保存在当前用户的 Apple Keychain，主界面会显示对应的 `Controller verify key`；Windows 首次配对会在 Noise 认证后展示该身份并要求本地批准，无需把控制端公钥加入签名邀请。环境变量仍是开发联调入口，不替代最终扫码/粘贴配对 UI。
