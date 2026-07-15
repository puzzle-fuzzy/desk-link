# DeskLink

DeskLink 是面向个人设备的跨平台远程桌面工具，目标是在自己的 Windows、Mac 和 iPhone 之间稳定查看并控制电脑桌面。

当前优先级：

1. Windows 桌面端；
2. macOS Apple Silicon 桌面端；
3. iOS 控制端。

项目设计资料位于 [`docs/`](docs/)，原始需求文档已原样归档。实现将采用 Rust 共享核心 + Windows 原生能力 + macOS 原生能力，iOS 在桌面端链路稳定后接入。

## 当前阶段

当前仓库已完成共享 Rust 核心、QUIC 中继、稳定 C ABI 和 macOS Apple Silicon 控制端骨架，并接通 Windows DXGI 采集、Media Foundation H.264 实时编码、VideoConfig、QUIC 视频发送、独立光标通道和可安全释放的 `SendInput` 边界。Windows runtime 已使用 DPAPI 持久保存 Ed25519 设备身份、可信控制端列表及连接配置，并在能力协商、采集和输入注入之前完成 Noise XX 双向认证及本地授权。默认首次运行使用现代连接设置界面；会话 ID、relay 加入密钥及网络参数经 Rust 校验后写入当前用户保护的 `connection.bin`，relay 密钥不会回显。一次性 `PairingInvite` 会分别携带 relay 加入密钥、host 身份和有效期；Noise 签名同时绑定设备 ID、公钥和握手 transcript。Windows host 现已提供真实本地审批对话框，完整展示设备 ID、公钥指纹、会话 ID 和过期时间，默认选择拒绝；批准后才会持久化信任。后续新 relay 会话可从可信列表重连；未知设备、设备 ID 换钥、邀请过期、拒绝及损坏的 DPAPI 存储均失败关闭。host 与 controller 现已共同使用共享的有界指数退避策略（250 ms 起步、8 s 封顶、最多 6 次）。Windows host 在 relay 故障后会完整重建 QUIC、Noise、DXGI、编码器和输入注入边界，并为恢复后的安全会话分配递增 `stream_id`；稳定协商会重置重试预算，认证/授权、协议及本地采集编码错误则立即失败关闭。现代 Tauri 常驻托盘和状态窗口现已接入这些生命周期事件，集中展示连接、重试和停止状态以及完整可信控制端身份；支持刷新、连接设置、窗口关闭后继续驻留、单实例恢复和明确退出。窗口采用 Segoe UI 层级、Per-Monitor V2 DPI、键盘可达控件，并已在真实 Windows 桌面完成配置校验、空状态、窗口驻留和单实例恢复验收。生命周期及可信设备操作会写入限额轮转的 JSON Lines 诊断日志；日志只记录允许的状态字段，命名凭据和连续长十六进制值会先脱敏，状态窗口也不展示底层错误原文。可信列表损坏或暂时不可用时，主窗口继续运行并提供明确的内联错误和刷新入口。Windows 当前用户单文件安装器、正式应用图标、开机启动和安装/升级/卸载边界现已完成；程序文件与 `%LOCALAPPDATA%\DeskLink` 用户数据严格分离，默认卸载保留身份、信任、连接配置及诊断数据。发布构建链现可选择 Microsoft Artifact Signing 或证书存储中的公有代码签名证书，按“应用先签、辅助 host 后签、安装器最后签”的顺序执行 SHA-256/RFC 3161 签名并强制 Authenticode 验证。Rust FFI crate 的真实 QUIC/Noise `ControllerRuntime` 已进入可取消后台 worker，负责认证握手、加密能力协商、VideoConfig/光标解密、视频分片重组、关键帧恢复和加密输入发送。C ABI 可直接验证并解析固定 181 字节签名 `PairingInvite`，篡改、过期和长度错误均不会启动网络 worker；macOS 配置层会优先使用单个 `DESKLINK_PAIRING_INVITE`，旧的分字段开发入口仍保留。macOS Swift 端已接通 VideoConfig → Annex B SPS/PPS → VideoToolbox、Annex B access unit → AVCC、异步最新帧发布和 Metal 等比显示，并通过 Apple Keychain 持久保存控制端身份；Apple 原生编译在当前 Windows 开发机上保持后置。已确认的设计规格位于：

Windows 界面已完成混合架构第三阶段：`apps/windows-ui` 提供 Tauri 2 + Vanilla TypeScript 控制界面，并由同一 Tauri 进程直接托管现有 Rust host runtime。连接、重连、远程控制活跃和停止状态会同步更新主窗口与托盘提示；关闭窗口后进程继续驻留，重复启动只会聚焦现有实例。现代界面现在可以创建最长 600 秒有效的一次性配对邀请、显式复制并取消邀请，也可以按完整身份撤销可信控制端；配对批准和撤销仍使用默认选择“No”的 Win32 原生确认窗口。Rust 命令继续直接读写 DPAPI 连接配置与可信控制端存储，relay 密钥不会返回 WebView；保存配置、取消配对或成功撤销都会先安全停止当前 supervisor，再按最新安全状态恢复正常托管。DXGI、Media Foundation、QUIC/Noise、`SendInput` 和高风险确认仍在 Rust/Win32 安全边界内。当前用户安装器以 `DeskLink.exe` 为开始菜单和登录启动入口，同时保留 `desklink-windows.exe` 作为命令行开发辅助程序；应用与安装器都声明 Windows 10/11 compatibility manifest。

Windows 稳定性阶段现已完成可自动重复的本机验收：DXGI 会枚举所有已连接输出并选择桌面坐标包含 `(0, 0)` 的 Windows 主显示器，不再假定 adapter 0 / output 0；系统唤醒使用 Windows suspend/resume callback 通知，并在 3 秒内去重后完整重启 host runtime。当前用户数据路径通过 Windows Known Folder API 获取，不依赖 `LOCALAPPDATA` 环境变量。本机双显示器拓扑、连续两轮 relay 重启和 60 秒 Noise + Media Foundation H.264 + 光标链路均已通过。当前多显示器支持范围是“在多屏拓扑中可靠捕获主显示器”；整块虚拟桌面合成和运行时显示器切换尚未实现。

Windows 控制端现在也集成在同一个 `DeskLink.exe`：`Control a PC` 页面支持粘贴邀请、Windows 身份认证、本地批准后的 DPAPI 连接保存、一键重连、WebCodecs H.264 低延迟显示、远端光标、全屏以及键盘/鼠标/滚轮转发。配对邀请绑定 host 已保存的正常 relay 会话；控制端只会在收到批准后的 `VideoConfig` 后保存重连材料。配对 worker 结束后 host 会自动恢复正常托管。本机真实 DXGI/Media Foundation + Windows controller 集成测试已验证首次批准、视频/光标接收、关键帧请求以及沿用同一会话凭据的可信重连；第二台物理 Windows 电脑上的长时间网络验收仍列在下一阶段。

[`docs/superpowers/specs/2026-07-15-desklink-design.md`](docs/superpowers/specs/2026-07-15-desklink-design.md)

## 下一阶段（Windows 优先）

1. 在第二台真实 Windows/controller 设备上完成数小时运行、真实网络断开和实际睡眠唤醒验收，并决定是否加入显示器选择或虚拟桌面合成；
2. 取得发布代码签名身份，用正式证书生成签名包并在干净 Windows 虚拟机完成 SmartScreen、兼容性、升级和卸载验收；
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
cargo test -p desklink-windows-ui
python scripts/verify-windows-resilience.py --soak-seconds 60
cd apps/windows-ui && bun install && bun run build && bun run tauri build --no-bundle
cd ../..
cargo test --manifest-path tests/end-to-end/Cargo.toml
python scripts/build-windows-installer.py
python scripts/sign-windows-artifact.py --verify-only dist/windows/DeskLinkSetup-0.1.1-x64.exe
```

macOS arm64 的 Swift 输入映射、Rust FFI 链接和 VideoToolbox/Metal 编译检查已有通过记录。Windows MSVC 环境已通过真实桌面纹理 → NV12 → H.264 → VideoConfig/分片 → `QuicClient` → 本机 relay → `ControllerRuntime` 完整 access unit 的 runtime 冒烟，并验证 Noise 双向身份认证、分通道 AEAD、乱序数据报重放保护、配置前丢包后的关键帧恢复和独立光标 Datagram；relay 只接触业务密文及连接/通道元数据。独立双端集成测试还验证了加密滚轮输入和关键帧请求只能由 host 对应安全通道解密。DXGI 超时与 access-lost 使用稳定错误分类，runtime 会在 access-lost 后重建采集器并请求新 IDR。输入边界已覆盖精确桌面端点、水平/垂直滚轮、Shift/Control/Alt/Meta 组合键、Unicode 代理对、扩展方向键和部分注入失败后的安全释放；测试使用可注入后端，不会操作本机键鼠。

Windows 真实码流已经接入持续运行的 QUIC host loop，并完成端到端业务载荷加密。本机 relay 冒烟现已覆盖一次性邀请首次配对、本地批准、DPAPI 持久化，以及 Windows 控制端沿用同一 relay 会话凭据的可信重连；真实 relay 重启测试还确认 host 能重新建立端到端安全会话、重新初始化采集编码并切换到新视频流。负向测试确认用户拒绝、邀请过期、设备 ID 换钥、relay 认证不匹配和第二控制端会在采集与输入初始化之前被阻断；host 占用会按预算退避，而本地安全错误不会被重试掩盖。Windows 可执行入口已接入主机与控制端、一次性配对审批、常驻托盘、状态主窗口和可信设备撤销。relay 可通过 PEM 证书链和私钥提供 Windows 系统信任的生产 TLS，本机开发模式仍可回退到自签名证书。macOS 解码显示边界、安全 C ABI worker、Keychain 身份和 Swift 事件入口均已接线，并补充 Annex B/AVCC、显示几何、配置解析及 ABI 生命周期测试；macOS arm64 的 Swift/链接/打包和真实跨机验收延后到 Apple Silicon 环境统一执行。

## Windows 稳定性验收

运行 `python scripts/verify-windows-resilience.py --soak-seconds 60` 会依次验证当前 Windows 主显示器的真实 DXGI 帧、连续两轮 relay 故障后的全 runtime 重建、Windows suspend/resume callback 注册，以及本机 relay 上持续 60 秒的 Noise 加密 H.264 和光标传输。结构化结果写入 `dist/windows/windows-resilience-report.json`。该测试覆盖真实采集与硬件编码，但单机本地 relay 不等同于第二台物理设备上的真实网络，也不会自动让当前开发机进入睡眠；这两项保留为外部验收。

## 两台 Windows 电脑使用

两台电脑安装同一个 DeskLink 安装包，任意一台都可以作为 host 或 controller。`0.1.1` 起，同一局域网可直接使用内置 UDP 中继：host 保留默认连接设置并创建多行配对连接码，controller 完整粘贴后会自动取得主机局域网地址，host 本地确认完整身份后即可控制。跨网络使用仍需要两台机器都能访问、且 TLS 证书受 Windows 信任的公网 relay。“概览”会以稳定检查代码汇总双机连接状态，并可将已脱敏的中文诊断报告导出到 Windows“下载”文件夹。完整部署和故障检查见 [`docs/windows-two-pc-setup.md`](docs/windows-two-pc-setup.md)。

## Windows 安装包

在 Windows x64 开发机运行 `python scripts/build-windows-installer.py`，会生成 `dist/windows/DeskLinkSetup-0.1.1-x64.exe`。安装器为单文件、当前用户、无需管理员权限的 GUI 安装包：现代入口 `DeskLink.exe` 与配对辅助程序 `desklink-windows.exe` 安装到 `%LOCALAPPDATA%\Programs\DeskLink`，开始菜单快捷方式和 Apps & Features 注册会同步创建，默认在 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 写入仅含 `DeskLink.exe --startup` 的启动项。普通启动显示现代主窗口；登录启动只创建托盘并在后台启动 host，左键托盘或选择 `Open DeskLink` 可恢复窗口。

静默安装使用 `--quiet`，同时指定 `--no-autostart` 可关闭自动启动。默认卸载只删除程序、快捷方式和注册项，保留 `%LOCALAPPDATA%\DeskLink` 下的身份、连接配置、可信控制端和诊断数据；只有显式使用 `--remove-data` 才会删除这些数据。未设置签名环境变量时构建结果会明确标记为 `unsigned`；正式签名配置和命令见 [`docs/windows-code-signing.md`](docs/windows-code-signing.md)。

## Windows 现代控制界面

`apps/windows-ui` 是 Windows 的默认 Tauri 控制界面。它使用系统 WebView2 渲染，Rust 后端复用现有 `desklink-windows` 的 DPAPI、身份、信任、一次性配对 authorizer、host supervisor 和 Windows controller runtime；Tauri capability 只开放明确注册的 host/controller 命令与状态事件，不开放 shell 或文件系统。视频通过独立二进制 IPC channel 进入 WebCodecs，输入则先在 Rust 中完成协议边界校验。配对邀请只在用户显式创建后返回给当前窗口，窗口会在过期或取消时清除本地副本；邀请包含批准后用于重连的私有 relay join secret，因此只应交给自己的 DeskLink 控制端。可信设备撤销会显示完整设备身份并调用 Win32 默认拒绝确认，确认成功后立即重启 host，使活动连接不能继续沿用旧信任。开发构建使用：

```sh
cd apps/windows-ui
bun install
bun run build
bun run tauri build --no-bundle
```

输出位于 `target/release/desklink-windows-ui.exe`。安装器会把它部署为 `DeskLink.exe`，由它负责单实例、托盘、host 生命周期、连接设置、首次配对和可信设备管理；原生审批边界继续保留，不会把高风险确认降级为 WebView 对话框。

## Windows host 手动运行

现代 `DeskLink.exe` 的 “Pair a device” 是默认首次配对入口。命令行开发或诊断仍可设置 `DESKLINK_PAIRING_MODE=1`；辅助 host 会生成最长 600 秒有效的一次性签名邀请，并在本地终端输出 session、relay 加入密钥、host 公钥及完整邀请。这些值都是临时秘密，不应通过不可信渠道传播。控制端完成 Noise 身份认证后，Windows 会显示默认拒绝的本地审批对话框，只有确认同一设备身份后才会写入可信列表。

普通连接/可信重连使用以下参数：

- `DESKLINK_SESSION_ID`：32 个十六进制字符；
- `DESKLINK_AUTH_KEY`：64 个十六进制字符；
- `DESKLINK_PEER_VERIFY_KEY`：可选的开发回退控制端 Ed25519 公钥，64 个十六进制字符；未设置时只接受 DPAPI 可信列表中的控制端；
- `DESKLINK_APPROVE_SESSION=1`：仅在启用上述开发回退时必需；回退身份不会自动写入可信列表；
- `DESKLINK_RELAY_ADDR`：可选，默认 `127.0.0.1:4433`；
- `DESKLINK_RELAY_SERVER_NAME`：可选，默认 `localhost`；
- `DESKLINK_STREAM_ID`：可选，默认 `1`。
- `DESKLINK_MANAGE_TRUST=1`：逐项显示可信控制端并询问是否撤销，处理后直接退出，不启动远程会话。

Windows 长期身份默认保存在 `%LOCALAPPDATA%\DeskLink\identity.bin`，连接配置保存在 `%LOCALAPPDATA%\DeskLink\connection.bin`，可信控制端保存在 `%LOCALAPPDATA%\DeskLink\trusted-controllers.bin`；实际基础目录由 Windows Known Folder API 获取，因此开机启动或受限启动环境缺少 `LOCALAPPDATA` 环境变量时仍可定位当前用户数据。三者均由当前 Windows 用户的 DPAPI 保护并采用原子替换写入。普通首次运行或显式执行 `desklink-windows.exe --configure` 可保存连接设置；修改现有配置时 relay 密钥留空会保留原密钥。结构化诊断日志保存在 `%LOCALAPPDATA%\DeskLink\logs\host.jsonl`，单文件上限 512 KiB，最多保留 3 个轮转历史文件。诊断日志不写入 relay 加入密钥、配对邀请或私钥材料。环境变量配置继续作为开发覆盖入口，不会替代或自动修改 DPAPI 配置。

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
