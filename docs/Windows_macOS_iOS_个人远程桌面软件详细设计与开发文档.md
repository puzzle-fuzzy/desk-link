# Windows / macOS / iOS 个人远程桌面软件详细设计与开发文档

> 文档版本：V2.0  
> 文档日期：2026-07-14  
> 项目性质：个人自用、非商业化  
> 产品类型：跨平台基础远程桌面软件  
> 支持平台：Windows、macOS、iOS  
> 核心目标：稳定同步远程桌面，并支持鼠标、键盘和触控操作  
> 临时代号：DeskLink（仅作为开发代号，可随时更换）

---

## 1. 项目背景

本项目不再以模仿或对标任何现有远程软件为目标，也不需要账号体系、会员、设备收费、企业后台或复杂的商业功能。

项目只解决一个明确问题：

> 在自己的 Windows、Mac 和 iPhone 之间，稳定地看到另一台电脑的实时桌面，并能够使用鼠标、键盘或触控完成操作。

此前使用现有远程软件时，出现过远程画面没有持续刷新、桌面变化不同步、鼠标能够移动但画面停留在旧帧等问题。对于远程桌面软件，这属于核心能力失效。

因此，本项目的第一原则不是功能多，而是：

1. 远程画面必须持续更新；
2. 鼠标位置和画面必须一致；
3. 键盘与触控操作必须能够及时反馈；
4. 画面冻结后必须自动检测并恢复；
5. 分辨率、显示缩放或显示器状态变化后不能永久黑屏；
6. 弱网时可以降低画质，但不能长时间显示旧画面；
7. 断线后必须明确显示状态并能够自动重连。

---

## 2. 项目最终定位

### 2.1 一句话定义

一款面向个人设备的轻量远程桌面工具，让 Windows、Mac 和 iPhone 可以安全地连接并控制自己的电脑。

### 2.2 产品原则

- 个人设备优先；
- 不建立复杂账号系统；
- 不依赖第三方远程桌面核心；
- 不复制其他项目的代码或协议；
- 桌面同步正确性高于清晰度；
- 操作延迟高于画面观赏性；
- 第一版只做单用户、单会话；
- 优先自建中继，后续再增加 P2P；
- Windows 和 macOS 可作为被控端；
- Windows、macOS 和 iOS 可作为控制端；
- iOS 不作为完整可控制的被控端。

### 2.3 不做的事情

第一阶段明确不做：

- 用户注册与登录；
- 会员和付费系统；
- 企业组织和成员权限；
- 远程打印；
- 远程摄像头；
- 语音通话；
- 屏幕录制；
- 文件传输；
- 多人同时控制；
- 多显示器同时观看；
- 虚拟显示器；
- 游戏模式；
- 4K、120FPS；
- 全球节点；
- 浏览器网页远控；
- Linux、Android；
- iPhone 系统级被控。

这些功能都不是当前核心问题。

---

## 3. 平台能力与角色边界

## 3.1 平台角色矩阵

| 平台 | 作为控制端 | 作为被控端 | 第一版状态 |
|---|---:|---:|---|
| Windows | 支持 | 支持 | 完整支持 |
| macOS | 支持 | 支持 | 完整支持 |
| iOS | 支持 | 不支持完整控制 | 仅作为控制端 |
| iOS 屏幕分享 | 不适用 | 可选，仅观看 | 后续扩展 |

最终可以实现：

- Windows 控制 Windows；
- Windows 控制 Mac；
- Mac 控制 Windows；
- Mac 控制 Mac；
- iPhone 控制 Windows；
- iPhone 控制 Mac。

## 3.2 为什么 iOS 不作为完整被控端

iOS 可以通过 ReplayKit 和 Broadcast Upload Extension 将屏幕画面发送出去，但公开 SDK 没有提供类似 Windows `SendInput` 或 macOS `CGEvent` 的系统级触控注入能力。

普通 iOS 应用处于系统沙箱内，无法：

- 在主屏幕上替用户点击；
- 控制其他 App 的按钮；
- 向其他 App 注入任意触摸；
- 模拟系统级滑动；
- 在锁屏界面执行远程操作。

因此，iOS 被控端最多可以实现：

- 用户主动开启屏幕直播；
- 远端观看 iPhone 屏幕；
- 发送文字或语音指导；
- 在本项目 App 内进行有限交互。

它不能实现真正意义上的“远程控制整个 iPhone”。

本项目不通过私有 API、越狱或系统漏洞绕过这一限制。

---

## 4. 主要使用场景

### 场景 A：iPhone 控制 Windows

用户在外面使用 iPhone，连接家中的 Windows 电脑：

- 查看电脑桌面；
- 点击应用；
- 滚动网页；
- 输入文字；
- 使用常见快捷键；
- 处理简单文件和后台任务。

### 场景 B：iPhone 控制 Mac

用户使用 iPhone 连接自己的 Mac：

- 查看当前桌面；
- 打开应用；
- 操作鼠标；
- 输入文本；
- 切换窗口；
- 完成简单维护。

### 场景 C：Mac 控制 Windows

用户在 Mac 上连接 Windows：

- 使用鼠标和实体键盘操作；
- 适应窗口或按原始比例显示；
- 显示远端鼠标光标；
- 支持常见组合键。

### 场景 D：Windows 控制 Mac

用户在 Windows 上连接 Mac：

- 查看 Mac 桌面；
- 点击、拖动、滚动；
- 输入中英文；
- 使用 Command、Option 等远端修饰键。

### 场景 E：同平台控制

- Windows 控制 Windows；
- Mac 控制 Mac。

这两个场景用于验证基础稳定性和平台内部输入映射。

---

## 5. 第一版 MVP 范围

## 5.1 必须完成

### 设备与连接

- 每台设备首次启动生成本地设备身份；
- 展示设备名称；
- 展示临时连接码；
- 支持扫描二维码连接；
- 支持被控端接受或拒绝连接；
- 支持将控制端加入可信设备；
- 支持主动断开；
- 支持断线自动重连；
- 支持一个被控端同时只接受一个控制会话。

### 画面

- 捕获一块主显示器；
- 最高 1920×1080；
- 默认 30FPS；
- 最低可自动下降到 10FPS；
- H.264 编码；
- 支持码率自适应；
- 支持窗口适配；
- 支持原始比例；
- 支持远端分辨率变化；
- 支持横竖屏旋转；
- 支持 Retina 和 Windows DPI 缩放；
- 支持画面冻结监测；
- 支持自动请求关键帧；
- 支持解码器重建。

### 输入

- 鼠标移动；
- 左键；
- 右键；
- 双击；
- 拖动；
- 滚轮；
- 键盘按下与抬起；
- 常用组合键；
- Unicode 文本输入；
- iPhone 触控映射为鼠标；
- 连接中断时释放所有按键和鼠标按钮。

### 状态反馈

- 正在连接；
- 等待接受；
- 正在建立安全通道；
- 正在接收画面；
- 网络较差；
- 画面正在恢复；
- 已断开；
- 被控端拒绝；
- 权限不足；
- 屏幕采集权限未开启；
- 辅助功能权限未开启。

## 5.2 第二阶段再加入

- 文本剪贴板；
- 多显示器切换；
- 系统音频；
- 文件传输；
- 开机自动运行；
- 无人值守连接；
- Windows UAC 辅助服务；
- macOS 登录项；
- 局域网自动发现；
- P2P 直连；
- TLS/TCP 备用传输；
- iOS 屏幕只读分享；
- 远端分辨率临时调整。

---

## 6. 最关键的技术决策

## 6.1 不先做 P2P

第一版不把 NAT 穿透和 P2P 作为核心。

原因：

- P2P 连接成功率受路由器、运营商和 NAT 类型影响；
- 会显著增加排查难度；
- 很容易把“画面不同步”和“网络连接问题”混在一起；
- 个人使用的设备数量很少；
- 可以接受通过自己的服务器中继；
- 客户端全部主动连接服务器，不需要在家庭网络开放端口。

第一版采用：

> 客户端主动连接自建中继服务器，服务器只转发端到端加密的数据。

等桌面同步完全稳定后，再增加 STUN、ICE 和 P2P。

## 6.2 不使用网页 Canvas 作为核心远程画面渲染器

WebView 和普通网页适合设置页面，但远程桌面核心窗口需要：

- 快速解码；
- 低延迟显示；
- 丢弃旧帧；
- GPU 纹理渲染；
- 正确处理颜色空间；
- 精准映射鼠标坐标；
- 控制渲染队列长度。

因此远程画面使用系统原生 GPU 渲染：

- Windows：Direct3D 11；
- macOS：Metal；
- iOS：Metal。

## 6.3 不统一所有平台的屏幕采集代码

三种系统的采集和输入权限差异非常大。强行使用一个跨平台截图库，通常会带来黑屏、性能、权限和兼容问题。

正确做法是：

- 协议、加密、网络和会话逻辑共享；
- 屏幕采集使用系统原生 API；
- 视频编码和解码使用系统硬件能力；
- 输入注入使用平台原生 API；
- UI 按平台实现；
- 只在“编码后的数据”层面统一。

## 6.4 画面同步优先于完整帧率

画面不同步的常见根因并不是网络完全断开，而是：

- 采集线程停止产出；
- 编码器队列堆积；
- 旧帧仍在排队；
- 网络可靠传输等待丢失包；
- 解码器丢失关键帧后一直等待；
- 分辨率变化后编码器未重建；
- 控制端渲染队列过长；
- 鼠标被编码进画面导致位置滞后；
- 会话恢复后仍显示旧帧。

本项目采用：

> 最新帧优先，过期帧直接丢弃。

远程桌面不是电影播放。与其完整播放 3 秒前的旧画面，不如丢弃旧帧，立即显示最新桌面。

---

## 7. 总体技术架构

```text
┌──────────────────────────────────────────────────────────┐
│                    控制端应用                              │
│ Windows / macOS / iOS                                    │
│                                                          │
│ 输入采集 → 输入协议 → 加密 → 网络                         │
│ 网络 → 解密 → 视频组帧 → 硬件解码 → GPU 渲染              │
└──────────────────────────┬───────────────────────────────┘
                           │
                           │ QUIC / UDP 443
                           │ 端到端加密
                           ▼
┌──────────────────────────────────────────────────────────┐
│                    自建中继服务器                          │
│                                                          │
│ 会话匹配 / 连接码 / 心跳 / 限流 / 数据转发                │
│ 不解码桌面内容，不保存视频，不保存输入内容                 │
└──────────────────────────┬───────────────────────────────┘
                           │
                           │ QUIC / UDP 443
                           ▼
┌──────────────────────────────────────────────────────────┐
│                    被控端应用                              │
│ Windows / macOS                                          │
│                                                          │
│ 屏幕采集 → 硬件编码 → 视频分片 → 加密 → 网络              │
│ 网络 → 解密 → 输入事件 → 系统鼠标键盘注入                 │
└──────────────────────────────────────────────────────────┘
```

---

## 8. 代码与语言选择

## 8.1 共享核心

使用 Rust 实现：

- 会话状态机；
- 连接协议；
- 视频包头；
- 输入协议；
- 心跳；
- 重连；
- 端到端加密；
- 设备身份；
- QUIC 通信；
- 中继客户端；
- 数据统计；
- 日志；
- 错误码。

Rust 核心编译为：

- Windows 原生库；
- macOS 静态库；
- iOS 静态库。

通过稳定的 C ABI 与 Swift 或平台 UI 通信。

## 8.2 Windows

推荐：

- Rust；
- `windows-rs` 调用 Windows API；
- Direct3D 11；
- DXGI Desktop Duplication API；
- Media Foundation H.264 编解码；
- `SendInput` 注入鼠标和键盘；
- 原生桌面窗口；
- 后续增加 Windows Service。

不建议在第一版使用 Electron 作为核心控制窗口。

## 8.3 macOS

推荐：

- Swift；
- SwiftUI 负责普通界面；
- AppKit 负责精细窗口和键盘事件；
- ScreenCaptureKit；
- VideoToolbox；
- Metal；
- CoreGraphics `CGEvent`；
- Rust 静态库负责协议和网络。

## 8.4 iOS

推荐：

- Swift；
- SwiftUI；
- UIKit 处理复杂触控与软键盘；
- VideoToolbox 解码；
- Metal 渲染；
- Rust 静态库负责协议和网络；
- Keychain 保存设备身份；
- 相机仅用于扫描连接二维码。

## 8.5 中继服务器

使用 Rust：

- Tokio 异步运行时；
- QUIC 服务；
- 会话映射；
- 心跳；
- 限流；
- 端到端加密数据透传；
- JSON 或 TOML 本地配置；
- 不使用数据库；
- Docker 部署；
- 单实例起步。

个人版本不需要 Bun、Elysia、PostgreSQL 和 Redis。

---

## 9. 分层设计

## 9.1 平台层

负责系统相关功能：

```text
platform-windows
├── capture
├── encoder
├── decoder
├── renderer
├── input
├── permission
└── service

platform-macos
├── capture
├── encoder
├── decoder
├── renderer
├── input
├── permission
└── launch-agent

platform-ios
├── decoder
├── renderer
├── touch-input
├── keyboard
├── permission
└── qr-scanner
```

## 9.2 共享核心层

```text
core
├── identity
├── pairing
├── session
├── transport
├── protocol
├── crypto
├── video-packet
├── input-event
├── heartbeat
├── reconnect
├── metrics
└── logging
```

## 9.3 UI 层

只负责：

- 展示本机信息；
- 输入连接码；
- 扫描二维码；
- 确认连接；
- 展示远程画面；
- 转换鼠标和触控事件；
- 展示连接状态；
- 设置画质；
- 主动断开。

UI 不直接实现协议，不直接操作 Socket，也不保存长期密钥明文。

---

## 10. Windows 被控端详细设计

## 10.1 屏幕采集

第一版主方案：

> DXGI Desktop Duplication API + Direct3D 11。

采集流程：

1. 创建 D3D11 Device；
2. 枚举显示适配器；
3. 选择主显示器；
4. 创建 `IDXGIOutputDuplication`；
5. 循环调用 `AcquireNextFrame`；
6. 获取桌面 D3D11 Texture；
7. 获取鼠标位置和鼠标形状；
8. 将纹理交给编码器；
9. 调用 `ReleaseFrame`。

Desktop Duplication 能提供：

- 当前桌面帧；
- Dirty Rect；
- Move Rect；
- 鼠标位置；
- 鼠标形状。

第一版虽然可以获取 Dirty Rect，但先不做差分编码，仍然编码完整画面，以降低同步错误。

## 10.2 Windows Graphics Capture 备用路径

后续可以增加 Windows Graphics Capture，用于：

- 捕获单独窗口；
- 用户通过系统选择器选择窗口；
- 某些 Desktop Duplication 异常场景；
- 特殊多显示器场景。

第一版只捕获完整主桌面。

## 10.3 编码

使用 Media Foundation H.264 Encoder MFT。

默认参数：

```text
编码：H.264
分辨率：1920×1080 或远端原始尺寸等比缩放
帧率：30FPS
目标码率：4Mbps
最低码率：800Kbps
最高码率：8Mbps
关键帧间隔：1 秒
B 帧：关闭
低延迟模式：开启
码率模式：优先 CBR，后续可使用受限 VBR
```

要求：

- 尽量保持 D3D11 GPU 纹理路径；
- 避免每帧从 GPU 拷贝到 CPU；
- 编码队列最大长度为 2；
- 队列满时丢弃较旧的未编码帧；
- 分辨率变化时重建编码器；
- 重建后第一帧必须为 IDR。

## 10.4 鼠标和键盘注入

使用 `SendInput`。

支持：

- 绝对坐标鼠标移动；
- 左右键按下与抬起；
- 中键；
- 垂直滚轮；
- 水平滚轮；
- 虚拟键码；
- 扫描码；
- Unicode 文本输入；
- 修饰键；
- 按键释放。

需要注意：

- 普通权限进程不能可靠控制更高完整性级别的窗口；
- UAC 和管理员窗口可能阻止输入；
- 第一版只保证普通桌面应用；
- 第二阶段增加提权辅助服务；
- 断线时必须释放 Ctrl、Alt、Shift、Win 和所有鼠标按钮。

## 10.5 Windows 显示缩放

网络协议中的鼠标位置统一使用归一化坐标：

```text
x: 0.0 ～ 1.0
y: 0.0 ～ 1.0
```

被控端根据实际虚拟桌面坐标转换：

```text
actual_x = desktop_left + normalized_x × desktop_width
actual_y = desktop_top  + normalized_y × desktop_height
```

不能直接发送控制端窗口像素坐标，否则在 125%、150% DPI 和不同分辨率下会偏移。

---

## 11. macOS 被控端详细设计

## 11.1 屏幕采集

使用 ScreenCaptureKit。

基本对象：

- `SCShareableContent`；
- `SCDisplay`；
- `SCContentFilter`；
- `SCStreamConfiguration`；
- `SCStream`；
- `SCStreamOutput`。

采集流程：

1. 检查屏幕录制权限；
2. 查询可共享显示器；
3. 选择主显示器；
4. 创建 Content Filter；
5. 排除本应用窗口，避免无限镜像；
6. 创建 Stream Configuration；
7. 设置分辨率和帧率；
8. 启动 `SCStream`；
9. 接收 `CMSampleBuffer`；
10. 将帧交给 VideoToolbox 编码。

建议配置：

```text
目标输出：1920×1080 以内
帧率：30FPS
显示鼠标：关闭
音频：第一版关闭
队列深度：小
像素格式：BGRA 或适合 VideoToolbox 的格式
```

鼠标单独通过输入/光标通道发送，不编码进画面。

## 11.2 macOS 编码

使用 `VTCompressionSession`。

建议参数：

```text
Codec：H.264
RealTime：true
AllowFrameReordering：false
ExpectedFrameRate：30
MaxKeyFrameInterval：30
AverageBitRate：4Mbps 起步
Profile：兼容优先
```

输出 Annex B 或统一的长度前缀 NAL 单元，在共享核心层转换为项目协议。

## 11.3 macOS 输入控制

使用 CoreGraphics 事件：

- `CGEvent`；
- 鼠标移动事件；
- 鼠标按下与抬起；
- 滚轮；
- 键盘事件；
- 事件发布到系统事件流。

被控端必须获得：

- 屏幕与系统音频录制权限；
- 辅助功能权限。

没有辅助功能权限时：

- 可以继续显示桌面；
- 必须禁用远程输入；
- 控制端显示“只能观看”；
- 提供清晰的权限引导。

## 11.4 macOS 特殊边界

第一版不保证：

- FileVault 登录界面；
- 系统启动前控制；
- 未登录用户会话；
- 安全输入状态下的全部按键；
- 所有系统授权弹窗；
- 跨用户切换；
- 锁屏后的完整恢复。

第一版目标是控制已经登录的普通桌面会话。

---

## 12. iOS 控制端详细设计

## 12.1 远程画面

流程：

1. QUIC 接收视频分片；
2. 按 `frame_id` 组帧；
3. 验证分片完整性；
4. 丢弃过期或不完整帧；
5. 解析 H.264 参数集；
6. 使用 `VTDecompressionSession` 解码；
7. 得到 `CVPixelBuffer`；
8. 通过 Metal 渲染；
9. 根据设备方向重新计算显示区域。

## 12.2 触控映射

默认控制模式：

| iPhone 手势 | 远端操作 |
|---|---|
| 单指轻点 | 鼠标左键点击 |
| 单指双击 | 鼠标左键双击 |
| 单指拖动 | 移动鼠标 |
| 轻点后停顿再拖动 | 鼠标按住拖拽 |
| 双指上下滑动 | 鼠标滚轮 |
| 双指轻点 | 鼠标右键 |
| 长按 | 右键或打开操作菜单 |
| 双指缩放 | 仅缩放本地远程画面 |
| 三指轻点 | 显示控制工具栏 |

建议提供两种触控模式：

### 直接触控模式

点击画面位置，鼠标立即移动并点击对应位置。

适合：

- 大按钮；
- 平板式操作；
- 简单应用。

### 触控板模式

手指滑动控制鼠标相对移动，轻点执行点击。

适合：

- 小按钮；
- 精细操作；
- 桌面软件；
- 长时间使用。

## 12.3 软键盘

使用隐藏的 `UITextView` 或专用输入组件接收文本。

需要支持：

- 中文输入法最终文本；
- 英文；
- 数字；
- 删除；
- 回车；
- Tab；
- Escape；
- 方向键；
- Ctrl；
- Alt；
- Shift；
- Windows 键；
- Command；
- Option。

界面提供一排特殊键：

```text
Esc | Tab | Ctrl | Alt/Option | Win/Command | Shift | ↑ ↓ ← →
```

对于普通文字，优先发送 Unicode 文本事件；对于快捷键，发送物理按键事件。

## 12.4 iOS 前后台

iOS 控制端进入后台后：

- 暂停视频解码；
- 通知被控端降低或暂停视频；
- 保持短时间会话状态；
- 返回前台后请求新的关键帧；
- 不继续播放后台积压的旧帧；
- 超过会话保留时间后重新建立安全会话。

---

## 13. 控制端渲染设计

## 13.1 Windows 控制端

- Media Foundation 或 D3D11 Video Decoder；
- 解码输出保留在 GPU；
- Direct3D 11 Swap Chain；
- 垂直同步可关闭或使用低延迟提交；
- 渲染队列只保留最新 1～2 帧；
- 不允许播放器式长缓冲。

## 13.2 macOS 控制端

- `VTDecompressionSession`；
- `CVPixelBuffer`；
- `MTKView`；
- Metal 纹理缓存；
- 处理 BGRA/NV12；
- 保持正确宽高比；
- 计算 Letterbox 区域；
- 鼠标坐标只映射到有效画面区域。

## 13.3 iOS 控制端

与 macOS 共用大部分解码与 Metal 渲染代码，但 UI 和输入映射独立。

---

## 14. 解决“桌面不同步”的核心机制

这是本项目最重要的一章。

## 14.1 每帧必须带唯一编号

```rust
struct VideoFrameHeader {
    protocol_version: u16,
    stream_id: u32,
    config_version: u32,
    frame_id: u64,
    capture_timestamp_us: u64,
    width: u16,
    height: u16,
    flags: u16,
    chunk_index: u16,
    chunk_count: u16,
    payload_length: u32,
}
```

重要字段：

- `frame_id`：严格递增；
- `config_version`：分辨率或编码配置变化时递增；
- `capture_timestamp_us`：计算端到端延迟；
- `flags`：关键帧、配置帧、普通帧；
- `chunk_index`：当前分片；
- `chunk_count`：当前帧总分片数。

## 14.2 最新帧优先

各级队列采用有界队列：

```text
采集 → 编码：最多 2 帧
编码 → 网络：最多 2 帧
网络 → 组帧：最多 3 帧
解码 → 渲染：最多 2 帧
```

当队列已满：

- 丢弃最旧的普通帧；
- 不丢关键帧配置；
- 优先保留最新帧；
- 记录丢帧统计；
- 不阻塞采集线程。

## 14.3 禁止旧帧倒退显示

控制端保存：

```text
last_presented_frame_id
```

只有当：

```text
incoming_frame_id > last_presented_frame_id
```

才允许显示。

重连后必须更换 `stream_id`，防止旧会话的延迟包进入新会话。

## 14.4 关键帧恢复

以下情况立即请求 IDR：

- 首次连接；
- 解码器初始化；
- 丢失关键参数；
- 连续多帧无法解码；
- 检测到视频分片丢失；
- 网络恢复；
- App 返回前台；
- 分辨率变化；
- 显示器变化；
- 解码器重建；
- 画面冻结监测触发。

被控端收到 `RequestKeyframe` 后：

- 清空待发送的旧普通帧；
- 强制编码下一帧为 IDR；
- 附带 SPS/PPS；
- 更新配置版本。

## 14.5 画面冻结监测

控制端维护三个时间：

- 最后收到视频包时间；
- 最后成功解码时间；
- 最后成功显示新帧时间。

判断：

```text
有心跳但 500ms 没有新画面：
    发送 VideoProbe

有视频包但 800ms 没有成功解码：
    重建解码器并请求关键帧

没有视频包但心跳正常：
    通知被控端重启采集/编码通道

心跳也中断：
    进入重连状态
```

被控端维护：

- 最后采集帧时间；
- 最后编码完成时间；
- 最后发送视频时间。

如果桌面确实没有变化，也应发送轻量 `VideoAlive` 消息，避免控制端误判采集冻结。

## 14.6 分辨率变化

被控端检测以下变化：

- 显示器分辨率；
- Windows DPI；
- macOS Retina Scale；
- 横竖屏旋转；
- 显示器断开；
- 睡眠恢复。

处理顺序：

1. 暂停发送普通帧；
2. `config_version + 1`；
3. 重建采集纹理；
4. 重建编码器；
5. 发送 `StreamConfigChanged`；
6. 控制端清空组帧和解码队列；
7. 控制端重建解码器和渲染尺寸；
8. 被控端发送 IDR；
9. 恢复普通帧。

## 14.7 鼠标独立通道

鼠标不依赖视频帧。

被控端发送：

```rust
struct CursorState {
    sequence: u64,
    x: f32,
    y: f32,
    visible: bool,
    shape_id: u64,
}
```

鼠标位置可以以 60Hz 发送，即使视频只有 20～30FPS，控制端也能看到更及时的鼠标变化。

鼠标形状只有变化时才发送，使用 `shape_id` 缓存。

## 14.8 输入立即发送

输入事件使用可靠、有序控制流，不与视频数据混用。

每个事件包含：

```rust
struct InputEnvelope {
    sequence: u64,
    timestamp_us: u64,
    event: InputEvent,
}
```

被控端：

- 按序处理；
- 去重；
- 记录最后序列号；
- 断线时释放按键；
- 不因视频拥塞阻塞输入。

---

## 15. 网络协议设计

## 15.1 传输协议

第一版：

- QUIC；
- UDP 443；
- 客户端主动连接；
- 控制消息使用可靠流；
- 输入使用可靠流；
- 视频使用 QUIC Datagram；
- 心跳使用独立控制消息；
- 服务器只转发。

如果实际网络大量阻止 UDP，再增加：

- TLS over TCP 443；
- WebSocket 443 备用通道。

## 15.2 通道划分

| 通道 | 可靠性 | 内容 |
|---|---|---|
| Session Control | 可靠、有序 | 建连、接受、拒绝、断开 |
| Input | 可靠、有序 | 鼠标、键盘、触控 |
| Video Config | 可靠、有序 | 编码参数、SPS/PPS、分辨率 |
| Video Frame | 不可靠、低延迟 | H.264 帧分片 |
| Cursor | 可丢弃、低延迟 | 鼠标位置与形状 |
| Metrics | 可丢弃 | RTT、码率、丢帧、解码时间 |
| Heartbeat | 可靠或周期消息 | 在线检测 |

## 15.3 为什么视频不用单一可靠流

如果一个视频分片丢失，可靠流会等待重传，后面的新帧也可能被旧数据拖住。

远程桌面更适合：

- 丢掉不完整旧帧；
- 等下一个关键帧或完整新帧；
- 输入不被视频影响；
- 控制端不显示积压内容。

## 15.4 视频分片

建议单个 Datagram 控制在安全大小内，例如约 1000～1200 字节有效负载，避免 IP 分片。

帧组装规则：

- 按 `stream_id + frame_id` 建立临时缓冲；
- 收齐全部分片才交给解码器；
- 超过最大等待时间立即丢弃；
- 非关键帧丢失不重传；
- 视频配置和关键参数走可靠通道；
- 关键帧严重丢失时请求新的 IDR。

---

## 16. 中继服务器设计

## 16.1 职责

中继服务器只负责：

- 接收客户端连接；
- 验证临时会话；
- 匹配控制端与被控端；
- 转发加密数据；
- 心跳；
- 限制单会话带宽；
- 清理失效会话；
- 输出基础日志。

不负责：

- 屏幕解码；
- 视频存储；
- 鼠标内容分析；
- 用户数据库；
- 文件存储；
- 账号系统；
- 密码明文保存。

## 16.2 会话结构

```rust
struct RelaySession {
    session_id: SessionId,
    host_connection: Option<ConnectionId>,
    controller_connection: Option<ConnectionId>,
    created_at: Instant,
    expires_at: Instant,
    state: RelaySessionState,
}
```

状态：

```text
Created
WaitingForHost
WaitingForController
PendingApproval
Connected
Reconnecting
Closed
Expired
```

## 16.3 部署

```text
服务器：
- Linux
- Docker
- UDP 443
- TCP 443（未来备用）
- 日志目录
- 配置目录
```

初期单台服务器足够。

中继服务器配置：

```toml
listen_quic = "0.0.0.0:443"
session_ttl_seconds = 600
heartbeat_timeout_seconds = 15
max_sessions = 20
max_session_mbps = 20
log_level = "info"
```

---

## 17. 设备身份与安全

## 17.1 设备身份

每台设备首次启动生成：

- 长期 Ed25519 身份密钥；
- 设备 ID；
- 设备名称；
- 创建时间。

保存位置：

- Windows：DPAPI 保护的本地存储；
- macOS：Keychain；
- iOS：Keychain。

私钥不上传到服务器。

## 17.2 临时连接

被控端生成：

- 随机会话 ID；
- 高强度随机连接令牌；
- 便于手输的短码；
- 二维码；
- 过期时间。

二维码包含完整连接信息；短码只用于向服务器查找临时会话，不能直接作为加密密钥。

## 17.3 首次连接流程

```text
1. 被控端生成临时会话
2. 控制端输入短码或扫描二维码
3. 两端通过中继交换临时公钥
4. 建立端到端加密握手
5. 两端显示对方设备名称和指纹
6. 被控端接受或拒绝
7. 接受后启动视频和输入通道
```

## 17.4 端到端加密

建议使用成熟协议组合，不自行设计加密算法。

可选：

- Noise Protocol Framework；
- X25519 临时密钥交换；
- ChaCha20-Poly1305；
- Ed25519 身份签名；
- BLAKE2s 或 SHA-256。

中继服务器只能看到：

- 连接时间；
- 数据包大小；
- 会话持续时间；
- 客户端 IP；
- 带宽。

不能看到：

- 桌面画面；
- 键盘内容；
- 鼠标内容；
- 连接密钥。

## 17.5 可信设备

用户可将控制端加入本地可信列表。

可信设备仍然需要：

- 设备签名验证；
- 会话加密；
- 可随时撤销；
- 被控端显示当前连接；
- 一键断开；
- 可关闭免确认。

第一版可以先不做无人值守，所有连接必须在被控端点击接受。

---

## 18. 自适应画质

## 18.1 控制端上报

每秒上报：

- RTT；
- 接收码率；
- 视频分片丢失率；
- 完整帧率；
- 解码失败数；
- 解码耗时；
- 渲染耗时；
- 当前显示延迟；
- 丢弃旧帧数量。

## 18.2 被控端调节

简单规则：

```text
网络良好：
1080p / 30FPS / 4～8Mbps

RTT 上升或丢包轻微：
1080p / 20FPS / 2～4Mbps

网络较差：
720p / 15FPS / 1～2Mbps

网络很差：
540p / 10FPS / 500K～1Mbps
```

优先级：

1. 保持输入可用；
2. 保持画面最新；
3. 降低码率；
4. 降低帧率；
5. 最后才降低分辨率。

不能通过无限增加缓冲来“保证流畅”。

---

## 19. 输入协议

```rust
enum InputEvent {
    PointerMove {
        x: f32,
        y: f32,
    },
    PointerButton {
        button: MouseButton,
        pressed: bool,
    },
    PointerWheel {
        delta_x: f32,
        delta_y: f32,
    },
    Key {
        code: KeyCode,
        scan_code: u32,
        pressed: bool,
        modifiers: Modifiers,
    },
    Text {
        text: String,
    },
    ReleaseAll,
}
```

## 19.1 按键映射

协议不要直接使用平台私有键码作为唯一表示。

同时保存：

- 通用逻辑键；
- 物理扫描码；
- Unicode 文本；
- 修饰键状态。

这样可以区分：

- 输入字母；
- 输入中文；
- Ctrl+C；
- Command+C；
- Windows 键；
- Option；
- 功能键；
- 方向键。

## 19.2 Windows 与 Mac 修饰键

控制端提供两种模式：

### 自动映射

- Mac Command → Windows Ctrl；
- Windows Ctrl → Mac Command；
- Option ↔ Alt。

### 原样发送

远端按键与本地按键名称一致。

用户可在会话工具栏切换。

---

## 20. 项目目录建议

```text
desklink/
├── Cargo.toml
├── README.md
├── LICENSE
├── docs/
│   ├── 01-product-scope.md
│   ├── 02-protocol.md
│   ├── 03-video-pipeline.md
│   ├── 04-input-mapping.md
│   ├── 05-security.md
│   ├── 06-platform-windows.md
│   ├── 07-platform-apple.md
│   └── 08-testing.md
├── crates/
│   ├── desklink-core/
│   ├── desklink-protocol/
│   ├── desklink-crypto/
│   ├── desklink-transport/
│   ├── desklink-session/
│   ├── desklink-video-packet/
│   ├── desklink-input/
│   ├── desklink-ffi/
│   └── desklink-test-support/
├── apps/
│   ├── windows/
│   │   ├── src/
│   │   ├── capture/
│   │   ├── codec/
│   │   ├── renderer/
│   │   ├── input/
│   │   └── installer/
│   └── apple/
│       ├── DeskLink.xcworkspace
│       ├── Shared/
│       ├── macOS/
│       │   ├── Capture/
│       │   ├── Codec/
│       │   ├── Renderer/
│       │   ├── Input/
│       │   └── Permission/
│       └── iOS/
│           ├── Viewer/
│           ├── Touch/
│           ├── Keyboard/
│           └── QRScanner/
├── server/
│   ├── relay/
│   ├── Dockerfile
│   └── compose.yaml
├── scripts/
│   ├── build-windows.ps1
│   ├── build-apple.sh
│   ├── build-ios.sh
│   └── deploy-relay.sh
└── tests/
    ├── protocol/
    ├── transport/
    ├── video/
    ├── input/
    ├── reconnect/
    └── end-to-end/
```

---

## 21. 会话状态机

```text
Idle
  ↓
CreatingSession / ResolvingCode
  ↓
ConnectingRelay
  ↓
SecureHandshake
  ↓
WaitingForApproval
  ↓
NegotiatingCapabilities
  ↓
StartingVideo
  ↓
Connected
  ├── Degraded
  ├── RecoveringVideo
  ├── Reconnecting
  └── Disconnecting
  ↓
Closed
```

任何错误都必须落到明确状态，不允许 UI 一直停留在“连接中”。

---

## 22. 能力协商

连接成功后交换：

```rust
struct DeviceCapabilities {
    platform: Platform,
    role: DeviceRole,
    max_decode_width: u16,
    max_decode_height: u16,
    max_fps: u16,
    codecs: Vec<Codec>,
    supports_cursor_channel: bool,
    supports_text_input: bool,
    supports_physical_keys: bool,
    supports_clipboard: bool,
    supports_audio: bool,
}
```

第一版只协商 H.264。

如果一端不支持要求的分辨率，则选择双方都支持的最高档位。

---

## 23. 错误码

```text
CONNECTION_CODE_INVALID
CONNECTION_CODE_EXPIRED
HOST_OFFLINE
HOST_REJECTED
RELAY_UNREACHABLE
HANDSHAKE_FAILED
IDENTITY_MISMATCH
CAPTURE_PERMISSION_DENIED
ACCESSIBILITY_PERMISSION_DENIED
CAPTURE_START_FAILED
ENCODER_START_FAILED
DECODER_START_FAILED
VIDEO_TIMEOUT
VIDEO_CONFIG_MISMATCH
INPUT_INJECTION_BLOCKED
SESSION_REPLACED
NETWORK_TIMEOUT
RECONNECT_FAILED
UNSUPPORTED_CODEC
UNSUPPORTED_PLATFORM_ROLE
```

每个错误需要：

- 用户可理解的中文文案；
- 技术日志；
- 是否可重试；
- 建议操作；
- 平台信息。

---

## 24. 日志与诊断

## 24.1 日志分类

```text
app
session
transport
crypto
capture
encoder
packetizer
decoder
renderer
input
permission
relay
metrics
```

## 24.2 每次会话记录

仅本地记录：

- 会话 ID；
- 对端设备名称；
- 开始与结束时间；
- 平台；
- 平均 RTT；
- 平均码率；
- 实际帧率；
- 丢帧；
- 解码失败；
- 重连次数；
- 关键帧请求次数；
- 冻结恢复次数；
- 结束原因。

不记录：

- 屏幕图像；
- 键盘文本；
- 剪贴板内容；
- 输入的密码。

## 24.3 诊断面板

开发版远程窗口显示：

```text
RTT
Capture FPS
Encode FPS
Send Mbps
Receive Mbps
Complete FPS
Decode FPS
Present FPS
Dropped Frames
Last Frame ID
Video Delay
Input Sequence
Stream ID
Config Version
```

没有这套诊断数据，很难定位“不同步”到底发生在哪一层。

---

## 25. 开发阶段

## 阶段 0：本地媒体链路验证

目标：

- Windows 能持续捕获桌面；
- macOS 能持续捕获桌面；
- H.264 编码和解码可独立工作；
- 本地回环显示无永久冻结；
- 分辨率变化可恢复。

交付：

- Windows Capture Demo；
- macOS Capture Demo；
- Windows H.264 Encode/Decode Demo；
- Apple VideoToolbox Encode/Decode Demo；
- 帧编号与延迟统计。

## 阶段 1：同机回环

```text
屏幕采集 → 编码 → 分片 → 解码 → 本地窗口显示
```

不经过网络。

用于验证：

- 采集；
- 编码；
- 分片；
- 解码；
- 渲染；
- 队列；
- 关键帧；
- 分辨率变化；
- 画面冻结检测。

## 阶段 2：局域网 Windows → Mac

目标：

- Windows 作为被控端；
- Mac 作为控制端；
- 局域网连接；
- 实时画面；
- 鼠标；
- 键盘；
- 重连；
- 权限提示。

这是第一条完整垂直链路。

## 阶段 3：iPhone 控制 Windows

目标：

- 复用 Apple 解码和 Metal 渲染；
- 增加 iOS 触控映射；
- 增加软键盘；
- 支持横屏；
- 支持触控板模式；
- 支持二维码连接。

## 阶段 4：macOS 被控端

目标：

- ScreenCaptureKit；
- VideoToolbox 编码；
- CGEvent 输入；
- 权限引导；
- Windows 和 iPhone 均可控制 Mac。

## 阶段 5：自建公网中继

目标：

- QUIC 中继；
- 两端主动连接；
- 短码匹配；
- 端到端加密；
- 心跳；
- 重连；
- 带宽限制；
- Docker 部署。

## 阶段 6：稳定性

重点处理：

- 睡眠恢复；
- 网络切换；
- Wi-Fi 与蜂窝切换；
- 分辨率变化；
- App 前后台；
- 编码器异常；
- 解码器异常；
- 服务器重启；
- 长时间运行；
- 内存增长；
- 按键卡住；
- 鼠标坐标偏移；
- 中英文输入。

## 阶段 7：个人日常可用

加入：

- 可信设备；
- 自动填写自己的服务器；
- 本地设备列表；
- 自动重连；
- 开机启动；
- macOS 登录项；
- Windows 后台服务；
- 文本剪贴板；
- 多显示器切换。

---

## 26. 测试矩阵

## 26.1 设备组合

| 被控端 | 控制端 |
|---|---|
| Windows | Windows |
| Windows | macOS |
| Windows | iOS |
| macOS | Windows |
| macOS | macOS |
| macOS | iOS |

## 26.2 Windows 环境

- Windows 10；
- Windows 11；
- 100% DPI；
- 125% DPI；
- 150% DPI；
- 1920×1080；
- 2560×1440；
- 单显示器；
- 双显示器；
- Intel 核显；
- NVIDIA；
- AMD；
- 笔记本合盖前后；
- 睡眠恢复。

## 26.3 Mac 环境

- Intel Mac；
- Apple Silicon；
- Retina；
- 外接显示器；
- 不同 Space；
- 全屏应用；
- 睡眠恢复；
- 屏幕录制权限开启和关闭；
- 辅助功能权限开启和关闭。

## 26.4 iPhone 环境

- 刘海屏；
- 灵动岛；
- 横屏；
- 竖屏；
- Wi-Fi；
- 蜂窝网络；
- 前后台切换；
- 来电或系统弹窗打断；
- 中文输入法；
- 英文输入法；
- 蓝牙键盘。

## 26.5 网络条件

- 同一局域网；
- 不同公网；
- 20ms RTT；
- 50ms RTT；
- 100ms RTT；
- 1% 丢包；
- 3% 丢包；
- 抖动；
- 临时断网；
- Wi-Fi 切换蜂窝；
- 中继服务器重启。

---

## 27. 性能目标

这些是开发验收目标，不是对所有网络环境的绝对承诺。

### 局域网

- 1080p；
- 30FPS；
- 输入到画面反馈中位数小于 150ms；
- 正常办公画面不连续停留旧帧超过 500ms；
- 分辨率变化后 1 秒左右恢复；
- CPU 使用可接受；
- 无持续内存增长。

### 普通互联网

在 RTT 约 50～80ms、网络稳定时：

- 720p～1080p；
- 15～30FPS；
- 输入到画面反馈目标小于 300ms；
- 网络恶化时主动降级；
- 不能通过播放旧帧伪造流畅；
- 连接恢复后立即请求新关键帧。

### 稳定性

- 连续运行数小时不崩溃；
- 断网恢复不需要重启 App；
- 编码器失败后可重建；
- 解码器失败后可重建；
- 鼠标按键不会卡住；
- 远端桌面变化时不会永久静止；
- 返回前台后不显示旧画面。

---

## 28. MVP 验收标准

只有满足以下条件，才算基础远程桌面完成。

### 画面

- 被控端移动窗口，控制端可持续看到；
- 播放普通网页动画时画面持续更新；
- 屏幕静止后再次变化，控制端立即恢复更新；
- 改变分辨率后不会永久黑屏；
- 从睡眠恢复后能够重新显示；
- 网络抖动后不会一直停留旧帧；
- 新帧不会被旧帧覆盖；
- 关键帧丢失后能够自动恢复。

### 鼠标

- 指针位置准确；
- DPI 不同时不偏移；
- 点击目标与画面一致；
- 拖拽不频繁中断；
- 滚轮方向正确；
- 断线后不会保持按下状态。

### 键盘

- 英文正常；
- 中文最终文本正常；
- 删除、回车和方向键正常；
- Ctrl、Alt、Shift 正常；
- Windows 与 Command 映射可用；
- 断线后修饰键不会卡住。

### iPhone

- 横竖屏正常；
- 画面可缩放；
- 触控板模式可精细操作；
- 双指滚动可用；
- 软件键盘可输入；
- 前后台切换后能够恢复最新画面。

### 安全

- 首次连接需要被控端接受；
- 中继服务器无法解码画面；
- 私钥不上传；
- 临时连接码自动过期；
- 被控端可以立即终止连接；
- 当前正在被控制必须有明显提示。

---

## 29. 主要风险

## 29.1 Windows UAC

普通进程可能无法控制管理员窗口和 UAC 安全桌面。

处理：

- MVP 明确限制；
- 后续增加受签名的 Windows Service；
- UI 与服务通过本机 IPC 通信；
- 服务只接受已认证本机会话命令。

## 29.2 macOS 权限

用户不授予屏幕录制权限时无法采集；不授予辅助功能权限时无法控制。

处理：

- 启动时检测；
- 分开显示两项权限；
- 提供跳转系统设置；
- 权限变化后重新检测；
- 允许只观看模式。

## 29.3 硬件编码兼容

不同 GPU 和驱动可能导致编码器失败。

处理：

- 编码器能力探测；
- 记录 GPU 和驱动；
- 支持重建；
- 提供软件编码备用路径；
- 软件路径先限制 720p/15FPS。

## 29.4 QUIC 被阻止

部分网络可能阻止 UDP。

处理：

- 第一版先验证实际使用网络；
- 后续增加 TCP/TLS 443 备用；
- 输入和控制优先恢复；
- 网络切换后重新握手。

## 29.5 Apple 签名与权限稳定性

macOS 和 iOS 对应用签名、Bundle ID 和权限记录敏感。

处理：

- 开发阶段固定 Bundle ID；
- 使用稳定签名；
- 不频繁更换可执行文件身份；
- iOS 使用正式开发签名安装到自己的设备；
- macOS 打包时保留一致权限声明。

---

## 30. 最终技术结论

这款软件不需要从第一天就成为一个庞大的远程桌面平台。

最合理的实现路线是：

```text
共享 Rust 协议与网络核心
        +
Windows 原生采集、编码、输入与渲染
        +
macOS 原生采集、编码、输入与渲染
        +
iOS 原生解码、渲染与触控
        +
一个只负责转发的自建 Rust 中继服务器
```

第一条真正应该完成的链路是：

```text
Windows 被控端
    ↓
屏幕采集
    ↓
H.264 硬件编码
    ↓
帧编号与低延迟分片
    ↓
局域网传输
    ↓
Mac 硬件解码
    ↓
Metal 显示
    ↓
鼠标和键盘反向控制
```

这条链路稳定后，iOS 控制端可以复用 Apple 侧的大部分视频解码和渲染能力；随后再补充 macOS 被控端。

整个项目最重要的验收标准只有一句：

> 远端桌面一旦发生变化，控制端必须尽快显示最新画面；任何旧帧、网络丢包、编码器异常和分辨率变化，都不能让画面永久停留在过去。

---

## 31. 官方技术依据

- Microsoft Desktop Duplication API：用于逐帧获取 Windows 桌面、Dirty Rect、Move Rect 和鼠标信息。
- Microsoft Windows Graphics Capture：用于捕获显示器或应用窗口。
- Microsoft `SendInput`：用于合成鼠标移动、点击和键盘事件，但受 Windows 权限完整性限制。
- Apple ScreenCaptureKit：用于高性能捕获 Mac 显示器、窗口和应用内容。
- Apple CoreGraphics `CGEvent`：用于 macOS 鼠标和键盘事件。
- Apple VideoToolbox：用于 macOS 和 iOS 硬件视频编码与解码。
- Apple ReplayKit：可用于用户主动发起的 iOS 屏幕录制或广播，但不提供系统级远程触控注入。
- Apple Accessibility 权限：macOS 被控端执行系统级输入控制时需要用户授权。
