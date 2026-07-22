# DeskLink

DeskLink 是面向个人设备的端到端加密远程桌面工具。当前仓库同时包含 Windows 10/11 x64 桌面端与 macOS Apple Silicon 桌面端；两端共享 Rust 协议、密码学、QUIC 传输、会话、视频和 C ABI 核心，中继只负责匹配会话与转发业务密文。

## 当前状态

### Windows 10/11 x64

- 同一个 Tauri 2 + TypeScript 中文桌面应用同时承担被控制端和控制端；
- DXGI 多显示器采集、带输入清理和超时恢复的屏幕切换、不同排列下的物理像素坐标映射、适应窗口与 1:1 画面缩放、1:1 本地拖动浏览、沉浸式原生全屏、Media Foundation H.264 实时编码与带迟滞的自适应画质；新会话默认自动画质，流畅、均衡和清晰档均保持 30 FPS，自动策略同时观察网络丢包、控制端解码积压和本地显示合并帧，严重积压时回到最新关键帧而不继续播放陈旧画面；数据报缺片或帧号断层会关闭当前 H.264 增量参考链并有界请求新关键帧，不把不可解码的后续增量帧继续交给画面组件；BGRA/RGBA 到 NV12 的热路径复用 2×2 像素并预计算缩放映射，减少主机逐帧处理抖动；远端光标更新按动画帧合并，避免高频信号争用 WebView2 主线程；
- QUIC/TLS 中继、Noise 双向身份认证，以及独立视频、光标和输入安全通道；
- 设备目录在密码验证成功后预检双方协议版本；不兼容时停止无意义重连，并明确提示应升级当前控制端还是目标电脑；
- Windows DPAPI 保护的设备身份、连接设置、已保存控制端连接、可信设备列表、未完成文件任务与等待发送队列恢复记录；
- 稳定设备 ID、临时/固定访问密码、主机原生批准、可信重连与明确撤销；
- 鼠标、滚轮、常用键、导航键、F1–F12、独立 Ctrl/Alt/Shift/Windows 修饰键、组合键与批量中文文字发送；每个实时输入绑定当前视频流，断线重连不会重放旧会话输入；
- 双向纯文本剪贴板、远程画面内的安全 `Ctrl+V` 智能粘贴、经远端确认的双向文件传输、多文件队列、实时速度与失败后的显式重新发送；活动任务和等待队列均可在应用或电脑重启后恢复，并绑定原目标设备以阻止误发到其他电脑；恢复队列始终暂停载入，只有用户点击“继续队列”才会发送；界面会实时区分等待队列“已由当前账户加密保存”和“仅保留到本次运行”，写盘或清除失败可原位重试且不会伪装成成功；移除、清空、继续和保护重试会等待 Rust 状态机确认，处理期间互斥并在断线或超时后给出准确结果；文件先写入“下载”目录同卷的隐藏暂存区，校验通过后再原子发布；
- 剪贴板与文件操作采用请求 ID 关联和有界等待：迟到响应不会覆盖新操作，无人确认、远端选择超时、接收停滞或最终校验确认丢失时会自动释放状态并保留重试入口；
- 被控端 Windows 系统输出声音、64 kbit/s Opus 压缩、单包丢失恢复、控制端低延迟播放，以及工具栏即时静音；
- 中继故障后的持续重连，以及 QUIC、Noise、采集、编码和输入边界完整重建；
- 用户主动开启的脱敏云端诊断、会话关联和断网自动补传；
- 设置页可检查 GitHub 正式 Windows 版本；只有稳定版本号、匹配的 x64 安装器和明确标记为已签名的发布清单同时通过验证时才显示升级入口；
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

当前明确不支持：麦克风/语音对讲、整块虚拟桌面合成、后台静默下载与自动安装更新，以及 Windows UAC 安全桌面和 `Ctrl+Alt+Delete` 注入。多显示器通过会话内切换当前屏幕实现；剪贴板采用用户主动触发的纯文本传输，不在后台自动监听。正式版本检查失败或发布清单不完整时只在设置页降级提示，不会影响远程控制。

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

python scripts/run-windows-cargo.py clippy --workspace --all-targets --all-features -- -D warnings
python scripts/run-windows-cargo.py test --workspace
python scripts/verify-windows-release.py
python scripts/verify-windows-resilience.py --soak-seconds 60
python scripts/verify-managed-relay.py
```

`verify-windows-release.py` 会检查版本一致性、前后端正式中继配置、冻结依赖安装、前端测试与构建、全部生产资源中的开发地址、`custom-protocol` x64 PE、哈希和大小，并输出 `dist/windows/windows-release-verification.json`。

应用图标作为版本化资源提交到 `apps/windows/assets`；`scripts/generate-windows-assets.py` 只用于设计资源发生变化时重新生成，发布门禁会直接验证 PNG/ICO 文件签名与哈希，不依赖 CI 临时安装图像处理库。

`verify-windows-resilience.py` 会验证真实 DXGI 帧、H.264 编码、连续 relay 故障恢复、Windows suspend/resume callback，以及本机加密媒体和光标持续传输。它不等同于第二台物理电脑上的实际网络与睡眠验收。

`verify-managed-relay.py` 使用产品 transport 客户端执行系统证书链和 QUIC 握手，结果写入 `dist/windows/managed-relay-verification.json`。GitHub Actions 另有定时外部握手监控。脱敏诊断的隐私边界、部署和排查方式见 [`docs/cloud-diagnostics.md`](docs/cloud-diagnostics.md)。

## 两台 Windows 电脑使用

两台电脑安装同一个 DeskLink 安装包。被控制电脑启用远程连接并生成临时密码或固定密码；控制电脑输入设备 ID 和访问密码后发起安全连接，再由主机核对完整身份并批准。新会话默认采用自动画质，流畅、均衡和清晰档都保持 30 FPS；自动模式会同时观察网络丢包、本机解码积压和 Canvas 显示合并帧，持续压力时降低远端负载，严重积压时保留最后已显示画面并从新的关键帧继续，避免越播越慢。数据报缺片或 H.264 帧号断层后，控制端会暂停不可依赖的增量帧并请求新关键帧；若恢复关键帧也丢失，会以最多每秒一次的频率重试。控制端与 WebView2 之间使用一个在途请求和一个待取帧的有界交付路径；界面短暂卡顿时不会在桌面 IPC 中无限堆积旧画面，发生溢出会从新关键帧安全恢复。若 WebView2 的视频拉取命令短暂失败，0.1.60 会按 100、250、500、1000、2000 毫秒退避并自动恢复，不再让一次瞬时 IPC 错误永久冻结画面。也可以在工具栏中手动指定画质，并在“适应”和“1:1”之间切换远程画面。1:1 下的“浏览画面”只移动本地视野，不会误操作远端。全屏会进入 Windows 原生窗口全屏，隐藏 DeskLink 窗框和普通导航，让远程画面占用整个显示器；工具栏显示 3 秒后自动收起，鼠标移到屏幕顶部会立即出现，按 `Esc` 或点击“退出全屏”即可返回。主工具栏只保留分辨率、当前画质和加密状态，详细帧数与丢包数据仍留在诊断系统。切换远端显示器会先释放仍按下的输入，失败或超时不会破坏当前会话。“Windows 键”可直接打开远程开始菜单，Ctrl/Shift 与鼠标组合操作会保持到实际松开。在远程画面中按 `Ctrl+V` 会先安全释放本次快捷键状态，再把本机纯文本剪贴板写入远端；只有收到远端成功确认后才执行粘贴。也可在“传输”面板中双向复制纯文本，或选择文件发送到被控电脑的“下载”文件夹，文件发送前始终需要被控端确认。文件因断网中断时会保留已校验的接收进度；连接恢复后由用户点击重试，并再次经过远端确认后从断点继续。0.1.60 继续使用协议 9，两台电脑推荐同步升级；公网中继无需升级。

完整步骤、输入范围与故障处理见 [`docs/windows-two-pc-setup.md`](docs/windows-two-pc-setup.md)。

0.1.61 继续使用协议 9，并针对控制端高频状态更新、鼠标输入队列、远程游标和 Canvas 绘制做了性能优化；公网中继无需升级。

0.1.62 继续使用协议 9，并加固主机异常退出与控制端会话结束后的命令边界；主机运行任务发生不可恢复错误时会明确显示停止原因，已结束的控制会话不会再接受迟到操作；公网中继无需升级。

0.1.63 继续使用协议 9；诊断报告现在会合并并按时间排列主机与控制端日志，明确标注事件来源，便于定位中继、视频交付和 WebView2 显示之间的故障边界；公网中继无需升级。

0.1.64 继续使用协议 9；控制端连接状态的标题变化只更新状态徽标，不再重建远程画面和 WebView2 解码器，减少稳定会话中的无意义卡顿；公网中继无需升级。

0.1.65 继续使用协议 9；短暂重连时保留最后一帧远程画面并显示恢复覆盖层，避免网络抖动直接退回连接页面和重新等待首帧；公网中继无需升级。

0.1.66 继续使用协议 9；将远程画面保留策略收敛为可测试的生命周期状态机，覆盖完整重连握手并在终态可靠释放画面资源；公网中继无需升级。

0.1.67 继续使用协议 9；控制端诊断新增显示帧率（百分之一帧/秒）和最大帧间隔，帮助区分网络交付、解码和本地渲染卡顿；公网中继无需升级。

0.1.68 继续使用协议 9；诊断报告会根据最近的中继、解码和显示指标生成脱敏性能摘要，直接给出下一步排查方向；公网中继无需升级。

0.1.69 继续使用协议 9；性能摘要只关联时间相近的同一段诊断事件，避免旧会话指标污染当前卡顿判断；公网中继无需升级。

0.1.70 继续使用协议 9；控制端视频交付诊断增加流 ID 关联，优先按准确会话匹配，旧版本日志仍使用时间窗口兼容；公网中继无需升级。

0.1.71 继续使用协议 9；控制端诊断新增显示合并帧计数，用于识别解码输出超过本地 Canvas 刷新能力的情况；公网中继无需升级。
0.1.72 继续使用协议 9；自动画质会把持续的本地显示合并帧转换为已有的播放压力反馈，连续压力时主动降低远端编码负载，手动指定画质不受影响；公网中继无需升级。
0.1.73 继续使用协议 9；远端光标信号改为每个动画帧最多提交一次并始终采用最新坐标，断开或切换会话时取消待提交更新；公网中继无需升级。
0.1.74 继续使用协议 9；1:1 浏览和双显示器切换时，控制端用滚动偏移修正缓存坐标，避免高频滚动事件反复读取布局；公网中继无需升级。
0.1.75 继续使用协议 9；全屏鼠标移动不再重复查询工具栏和匹配 `:hover`，改用缓存节点与事件目标判断，减少高频指针事件的主线程开销；公网中继无需升级。
0.1.76 继续使用协议 9；控制端对未标记关键帧的视频包使用无分配 NAL 类型扫描，减少 30 FPS 解码热路径的短生命周期数组；公网中继无需升级。

0.1.77 继续使用协议 9；Windows 主机端的视频发送队列改为直接取出最新编码帧并原地丢弃过期帧，避免每次发送唤醒创建临时数组，降低高帧率会话的分配和延迟抖动；公网中继无需升级。

0.1.78 继续使用协议 9；Windows 主机端将 H.264 访问单元直接编码为视频数据报，借用每个分片的编码缓冲，移除中间视频包和重复 payload 克隆，降低高分辨率会话的 CPU 与堆分配抖动；公网中继无需升级。

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
- `transfer-recovery.bin`：未完成文件任务的 DPAPI 加密恢复记录，最多保留 24 小时；
- `file-queue-recovery.bin`：等待发送队列的 DPAPI 加密恢复记录，最多 20 项并保留 24 小时；
- `logs/host.jsonl`：限额轮转且脱敏的诊断事件。
- `logs/controller.jsonl`：控制端连接阶段、重试与画面计数的脱敏事件；
- `diagnostics-sharing.enabled`：用户主动开启诊断共享后写入的非敏感开关标记。

敏感 Windows 二进制存储均由 DPAPI 保护并原子替换。macOS 敏感身份与连接材料使用 Keychain。连接码、relay 加入密钥、私钥、完整设备身份和文件完整路径不会写入诊断日志或暴露给 WebView；配对批准与撤销保持系统原生、默认拒绝的确认边界。文件接收失败时会区分远端拒绝、传输占用、权限、磁盘空间或源文件变化，不会把所有失败统一解释为用户拒绝。断点文件只保留在“下载”文件夹同卷的隐藏暂存目录，续传前校验已有前缀哈希，完整哈希验证通过后才会发布；接收前还会为下载盘保留 64 MB 安全空间，显式取消、无效数据和超过 24 小时的残留会被清理。控制端的活动任务和等待队列恢复记录同样最多保留 24 小时，并用完整设备身份绑定原目标；活动任务只在用户连接同一设备并点击重试后恢复，等待队列只会暂停载入并要求用户点击“继续队列”，不会在启动、重连或连接其他设备时静默发送文件。放弃恢复只清除加密状态和匹配传输 ID 的隐藏断点，不删除用户原始文件；排队源文件在发送前还会重新校验名称、大小和修改时间。

## 自建中继

生产 relay 需要受客户端系统信任的 TLS 证书链，并开放 UDP 监听端口。主要配置包括 `DESKLINK_RELAY_ADDR`、`DESKLINK_RELAY_CERT_PEM`、`DESKLINK_RELAY_KEY_PEM`、`DESKLINK_RELAY_SESSION_TTL_S` 和容量限制变量。没有 PEM 时生成的 `localhost` 自签名证书只用于自动化测试。

当前加入中继信封会携带非秘密的稳定设备参与者 ID、应用协议版本和目录注册信息：同一设备重连时可原子替换自己的旧连接，不同控制端仍受单控制端限制。目录查询只有在访问密码匹配后才比较双方协议版本，错误密码仍统一表现为设备不可用。涉及加入或目录协议升级时，必须先升级 relay，再发布使用新协议的客户端。

## 发布前仍需人工验收

1. 两台物理 Windows 电脑连续运行数小时，覆盖跨网、断网、Wi-Fi 切换和实际睡眠/唤醒；
2. 使用正式代码签名身份，在干净 Windows 10/11 上验证 SmartScreen、安装、升级、登录启动与卸载；
3. 两台 Apple Silicon Mac 完成 Screen Recording/Accessibility 授权、首次邀请配对、输入和断网恢复验收；
4. 为公网 relay 增加第二节点、容量与证书告警，并执行故障切换演练；
5. 在代码签名证书就绪后启用正式 Release，继续实现经过独立更新签名验证的自动安装，并验证多显示器在不同 DPI、缩放模式与排列下的坐标一致性。
