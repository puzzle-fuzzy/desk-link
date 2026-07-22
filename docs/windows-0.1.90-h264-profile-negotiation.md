# Windows 0.1.90：H.264 Profile 协商与回退

## 本次目标

在不改变现有公网中继、Noise 加密和视频帧协议的前提下，提高 Windows 桌面文字的清晰度，同时把硬件和 WebView2 的兼容性失败限制在视频配置切换内，不让一次解码失败升级成远程会话断开。

## 已实现

- 协议版本从 10 升到 11。`DeviceCapabilities` 现在携带 `h264Profiles`，并要求 `Main` 始终存在。
- Windows 控制端声明 `Main + High`；当前 macOS FFI 路径只声明 `Main`。
- Windows 主机在双方能力交集里优先选择 `High`，没有 High 时使用 `Main`。
- Media Foundation 创建 High Profile MFT 失败时，编码器立即重建 Main Profile；主机不会因为 High 不可用而停止服务。
- 控制端在收到 High SPS 后调用 WebCodecs `VideoDecoder.isConfigSupported`。WebView2 不支持时，控制端通过加密控制通道请求主机切回 Main，并请求新的关键帧。
- 已配置的控制端解码器发生 High Profile 解码停滞或错误时，也只触发一次 Main 回退请求，避免循环重连。
- 控制端脱敏诊断会记录实际 SPS Profile、探测状态、探测耗时和回退原因，便于区分“网络没收到帧”和“本机解码器不兼容”。
- Windows 编码器新增显式 `H264EncoderSettings::experimental_4k()` 和一个被忽略的 Media Foundation 初始化探针；它只用于局域网实验前确认硬件 MFT 上限，不会被默认画质或公网中继调用。

## 兼容边界

协议 12 是当前开发线的统一协议。两台开发中的 Windows 电脑应安装同一套构建产物；项目未正式发布，不保留旧协议互通层。公网中继服务只转发加密字节，不解析控制消息。

High Profile 不是强制项。任何一端不支持、编码器创建失败或解码器探测失败，最终都会回到 Main。这个回退保留当前会话和控制通道，不清除已建立的设备信任关系。

## 验证范围

- `cargo test --workspace`：Rust 工作区通过。
- `cargo test -p desklink-protocol -p desklink-windows --test encoder_contract`：协议与 Windows 编码器契约通过。
- `bun test`：前端 151 项通过。
- `bun run build`：TypeScript 类型检查与 Vite 生产构建通过。

## 后续顺序

1. 在两台 Windows 机器上用 0.1.90 验证 High→Main 回退、持续鼠标输入和双屏切换。
2. 在两台 Windows 机器上确认诊断字段能区分 High 支持、High 回退和 Main 直连（只记录脱敏枚举，不记录屏幕内容）。
3. 再评估局域网 4K 实验档；必须同步提高帧完整性策略和内存预算，不能只把尺寸上限改成 3840×2160。
4. 最后设计带身份绑定、候选地址探测和中继回退的可选 P2P 视频通道。
