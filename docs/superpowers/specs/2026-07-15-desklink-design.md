# DeskLink 远程桌面产品设计规格

> 文档状态：实现计划 Task 1–10 已落地，平台原生验收按环境分别记录
>
> 设计日期：2026-07-15
>
> 产品范围：Windows、macOS Apple Silicon、iOS

## 1. 产品目标

DeskLink 面向个人设备，解决在自己的 Windows、Mac 和 iPhone 之间稳定查看并控制电脑桌面的问题。第一原则是桌面变化必须持续同步，输入必须及时反馈，画面冻结、分辨率变化、网络抖动和断线都必须有检测和恢复路径。

第一版是单用户、单会话产品，不包含账号体系、会员、企业组织、文件传输、远程打印、远程音频、屏幕录制、多人控制、浏览器远控、Linux、Android 或 iOS 系统级被控能力。

## 2. 已确认的平台优先级

| 平台 | 控制端 | 被控端 | 优先级 |
|---|---:|---:|---|
| Windows | 支持 | 支持 | 第一优先级 |
| macOS Apple Silicon | 支持 | 支持 | 第一优先级 |
| iOS | 支持 | 不支持完整系统控制 | 最后制作 |

macOS 第一版只构建和验证 `arm64-apple-macos`，不提供 Intel 产物，也不把 Intel Mac 纳入兼容矩阵。

第一条完整垂直链路是 Windows 被控端到 macOS 控制端，随后补齐 macOS 被控端，最后制作 iOS 控制端。

## 3. 总体架构

采用 Rust 共享核心加平台原生实现：

```text
Windows 原生桌面端 ─┐
                    ├─ Rust 共享核心 ─ 自建加密中继
macOS 原生桌面端 ───┘
                    └─ Swift/Objective-C C ABI（为 iOS 预留）
```

Rust 共享核心负责设备身份、配对、安全握手、会话状态机、QUIC 通信、视频帧协议、输入协议、心跳、重连、关键帧恢复、指标、日志和错误码。

Windows 平台使用 Desktop Duplication、D3D11、Media Foundation 和 `SendInput`。macOS 使用 ScreenCaptureKit、VideoToolbox、Metal 和 `CGEvent`。平台 UI 不直接操作 Socket，不保存长期密钥明文，平台能力通过明确的 adapter/FFI 边界接入共享核心。

中继服务器只负责会话匹配、心跳、限流和端到端加密数据转发，不解码、不保存桌面画面、不分析输入内容。开发阶段提供本地回环和局域网测试模式；正式公网连接使用独立 Rust QUIC 中继。

## 4. 项目结构

```text
desklink/
├── docs/
│   ├── Windows_macOS_iOS_个人远程桌面软件详细设计与开发文档.md
│   └── superpowers/specs/2026-07-15-desklink-design.md
├── crates/
│   ├── desklink-protocol/
│   ├── desklink-crypto/
│   ├── desklink-transport/
│   ├── desklink-session/
│   ├── desklink-video/
│   └── desklink-ffi/
├── apps/
│   ├── windows/
│   └── macos/
├── server/relay/
├── tests/
└── scripts/
```

共享包保持纯协议、纯状态或明确的基础设施边界。平台采集、编码、解码、渲染、输入和权限代码不混入协议层。

## 5. 视频数据流

被控端视频链路：

```text
屏幕采集
→ 有界采集队列（最多 2 帧）
→ H.264 编码
→ 帧编号
→ 低延迟分片
→ 加密
→ QUIC Datagram
```

控制端视频链路：

```text
QUIC Datagram
→ 解密
→ 按 stream_id + frame_id 组帧
→ 丢弃超时或不完整旧帧
→ 保留最新 1～2 帧
→ 硬件解码
→ D3D11 或 Metal 渲染
```

视频帧必须携带 `stream_id`、递增的 `frame_id`、`config_version`、采集时间、分辨率、关键帧标记、分片序号和分片总数。控制端只允许显示比 `last_presented_frame_id` 更新的帧，重连后必须使用新的 `stream_id`。

默认目标为主显示器、最高 1920×1080、30FPS、H.264。网络变差时依次降低帧率、码率和分辨率，最低可降至 10FPS；禁止使用无限播放缓冲掩盖延迟。

通道划分如下：

| 通道 | 传输方式 | 内容 |
|---|---|---|
| Session Control | 可靠、有序 | 建连、接受、拒绝、断开 |
| Input | 可靠、有序 | 鼠标、键盘、触控 |
| Video Config | 可靠、有序 | 编码参数、SPS/PPS、分辨率 |
| Video Frame | 不可靠、低延迟 | H.264 帧分片 |
| Cursor | 低延迟、可丢弃 | 鼠标位置和形状 |
| Heartbeat | 周期消息 | 在线检测 |
| Metrics | 可丢弃 | RTT、码率、丢帧、解码统计 |

输入不与视频共用队列。鼠标光标走独立通道，目标更新频率为 60Hz。视频和输入拥塞互不阻塞。

## 6. 冻结检测与恢复

控制端维护最后收到视频包、最后成功解码和最后显示新帧的时间。

```text
500ms 没有新画面但心跳正常
→ VideoProbe

800ms 收到视频包但没有成功解码
→ 清空解码队列
→ 重建解码器
→ 请求关键帧

心跳正常但没有视频包
→ 请求被控端重启采集/编码通道

心跳中断
→ Reconnecting
→ 新建 stream_id
→ 重新握手并请求关键帧
```

被控端检测到显示器分辨率、DPI、Retina 比例、显示器连接状态或睡眠状态变化时，暂停普通帧、递增配置版本、重建采集和编码器、发送新配置，控制端清空旧组帧和解码队列后重建解码器，最终由被控端发送带 SPS/PPS 的 IDR。

## 7. 输入协议

输入事件使用归一化坐标和递增序列号，不直接把控制端窗口像素坐标发送到被控端：

```text
x: 0.0 ～ 1.0
y: 0.0 ～ 1.0
```

协议同时保留通用逻辑键、物理扫描码、Unicode 文本和修饰键状态，以区分普通文字、中文输入、Ctrl+C、Command+C、功能键和方向键。

支持鼠标移动、左/右/中键、双击、拖动、垂直/水平滚轮、键盘按下/抬起、Unicode 文本、常用组合键和 `ReleaseAll`。任何断线、拒绝、替换或结束流程都必须释放 Ctrl、Alt、Shift、Command、Win 以及所有鼠标按钮。

## 8. 安全、配对与权限

首次启动生成 Ed25519 长期身份密钥和设备 ID。私钥存储在 Windows DPAPI 或 Apple Keychain，不上传服务器。

临时配对包含随机会话 ID、高强度连接令牌、短码、二维码和过期时间。短码只用于查找会话，不直接作为加密密钥。首次连接时交换临时公钥、验证设备指纹并由被控端手动接受。

安全通道使用成熟协议组合：X25519 临时密钥交换、Ed25519 身份签名、ChaCha20-Poly1305 加密和 Noise 风格握手。中继只能看到连接元数据和密文流量。

macOS 被控端单独检测屏幕录制权限和辅助功能权限。缺少屏幕录制权限时无法提供画面；缺少辅助功能权限时可以只观看但禁用远程输入。Windows 第一版只保证已登录用户的普通桌面应用，不承诺 UAC 安全桌面或更高完整性级别窗口。

## 9. 会话状态与错误处理

正常状态机：

```text
Idle
→ CreatingSession
→ ConnectingRelay
→ SecureHandshake
→ WaitingForApproval
→ NegotiatingCapabilities
→ StartingVideo
→ Connected
```

恢复和退出状态包括 `Degraded`、`RecoveringVideo`、`Reconnecting`、`Disconnecting` 和 `Closed`。任何错误都必须进入明确状态，不能让 UI 永久停留在“连接中”。

错误使用结构化模型：

```text
code
message_zh
layer
retryable
recovery_action
platform_details
```

错误码覆盖连接码无效、会话过期、被控端拒绝、中继不可达、握手失败、身份不匹配、屏幕权限不足、辅助功能权限不足、采集失败、编码失败、解码失败、视频超时、输入被阻止、会话被替换和重连失败。

## 10. 桌面端界面

首页显示设备名称、在线状态、权限状态、临时连接码、二维码、有效期、等待连接和连接其他设备入口。不要求注册或登录。

远程会话窗口采用原生 GPU 画面区域，状态栏显示对端名称、连接状态、RTT、画质、帧率、网络质量和断开按钮。工具栏提供画质、原始比例/适应窗口、键盘映射、请求关键帧、诊断面板和结束会话。

第一版设置只包含设备名称、中继地址、默认画质、键盘映射、连接确认、日志目录和开发诊断开关。Windows 和 macOS 桌面控制端使用本地鼠标和实体键盘；触控板模式、软键盘和触控手势属于 iOS 阶段。

被控端始终显示当前控制者、会话时间和立即断开按钮。

## 11. 测试与验收

Rust 核心测试覆盖协议编解码、帧编号、旧帧丢弃、分片组装、超时清理、关键帧请求、配置版本、状态机、输入去重、坐标转换、加密握手、连接码过期和单控制会话限制。

中继测试覆盖会话匹配、过期、拒绝、超时清理、第二控制端拒绝、密文透传、重启恢复和带宽限制。

Windows 测试覆盖 Windows 10/11、100%/125%/150% DPI、不同分辨率、Desktop Duplication、Media Foundation、普通权限窗口、睡眠恢复和输入映射。macOS 只测试 Apple Silicon，覆盖 Retina、外接显示器、屏幕录制权限、辅助功能权限、ScreenCaptureKit、VideoToolbox、Metal、CGEvent 和睡眠恢复。

网络测试覆盖局域网、50～80ms RTT、1%/3% 丢包、抖动、临时断网、Wi-Fi 切换和中继重启。

桌面 MVP 必须满足：远程画面持续更新；鼠标位置准确；点击、拖动、滚轮和常用键盘可用；中文最终文本可用；冻结、关键帧丢失、分辨率变化、断网和前后台切换可以恢复；断线后不会遗留按键状态；权限不足时给出明确提示；被控端可以立即断开。

由于开发环境为 macOS，macOS Apple Silicon 端可以本机编译和验证；Windows 原生能力需要在 Windows 机器或虚拟机中完成最终验收。

## 13. 里程碑

## 11. 实现验证记录

已在 macOS Apple Silicon 开发环境验证：

- Rust workspace 格式化、Clippy、单元测试和协议/视频/会话/加密/传输/中继测试通过；
- 入站控制、输入和视频接收通道独立，洪峰不会互相阻塞；
- 中继连接上限和连接错误码在并发条件下保持稳定；
- `desklink.h` C ABI 的句柄生命周期、空指针校验、配对、控制事件、输入事件和 `ReleaseAll` 测试通过；
- macOS arm64 Swift 输入映射、Rust FFI 链接、VideoToolbox 和 Metal 编译检查通过；
- 确定性回环验证了旧帧丢弃后的关键帧恢复、最新帧优先和视频压力下输入仍可交付。

Windows 原生采集、Media Foundation H.264 编码、普通桌面输入和 PE 产物尚未在 Windows 机器上执行，不将交叉 `cargo check` 视为设备验收。

1. 共享协议、状态机、帧分片、输入协议和本地回环测试；
2. Windows 被控端屏幕采集和 H.264 编码；
3. macOS Apple Silicon 控制端解码、Metal 渲染和桌面输入；
4. 鼠标、键盘、重连、冻结恢复和分辨率恢复；
5. macOS Apple Silicon 被控端；
6. 自建公网 QUIC 中继与端到端加密；
7. iOS 控制端、触控模式、软键盘和二维码连接；
8. 可信设备、开机启动、剪贴板和多显示器等增强能力。
