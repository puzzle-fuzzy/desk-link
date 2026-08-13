# GitHub 同类项目架构对照

更新日期：2026-08-13

本文只提炼公开项目的架构取舍和工程实践，不复制其代码、协议或许可证边界。DeskLink 当前仍以 Windows 首发、Rust 安全核心、单一公网中继和端到端加密为产品约束。

## 参考项目

### RustDesk

[rustdesk/rustdesk](https://github.com/rustdesk/rustdesk) 将桌面端保持在 Rust 数据平面，并把视频编解码、屏幕采集、键鼠输入、剪贴板、服务端连接和 rendezvous/relay 分成清晰的模块。它同时支持直连探测和中继回落，并允许自托管服务。

DeskLink 已采用同一类边界：Rust 负责协议、Noise、AEAD、QUIC、恢复和有界队列；Windows 层只负责 DXGI、Media Foundation、SendInput、WASAPI、DPAPI 和原生批准；DirectLan 只优化视频，控制与文件通道仍经过加密中继。后续不应把协议状态机重新搬回 WebView，也不应为了追求“P2P”破坏稳定的 relay 回落。

### MeshCentral

[Ylianst/MeshCentral](https://github.com/Ylianst/MeshCentral) 以设备管理和远程运维为中心，将设备登记、批准、远程桌面和文件传输组合成一个可持续运营的模型。

DeskLink 当前不扩展成企业 RMM：不新增账号强依赖、多租户后台或无人值守策略。但它提醒我们把“连接成功”之外的运维信息做成明确的会话边界：设备身份、批准状态、当前通道、传输任务和断线恢复应可分别观察，不能用一个“在线/已中断”状态覆盖所有问题。下一阶段优先完善会话诊断和人工验收记录，不增加后台权限面。

### FreeRDP

[FreeRDP/FreeRDP](https://github.com/FreeRDP/FreeRDP) 公开展示 ABI 检查、静态分析、CodeQL、跨平台构建和安全策略等长期质量门禁。

DeskLink 已有 Rust fmt、Clippy、workspace 测试、Windows 发布验证、中继探针和来源绑定的候选包；仍应把安全扫描作为独立门禁逐步补齐：先建立 CodeQL/依赖审计基线，区分“新增漏洞阻断”和“历史平台依赖告警”，避免把非 Windows 依赖告警误当成 Windows 运行时回归。

## 可执行的下一阶段

1. **发布前**：继续保持单一 relay 回落路径，完成两台真实 Windows 的同网、跨网、断线恢复、双屏/DPI、剪贴板、文件和 4 小时长稳记录；完成 Authenticode 与 SmartScreen 验收。
2. **运维可靠性**：在不改变业务协议的前提下增加第二 relay 节点、健康检查和故障切换演练；readiness 必须同时绑定候选源码和实际线上 revision。
3. **安全质量**：新增独立 CodeQL/依赖审计工作流，最小化 GitHub Actions 权限并保留可追溯的扫描摘要；扫描失败先按新增问题分级，不放宽正式发布的签名和人工设备门禁。
4. **体验观察**：把会话状态、当前视频路径、传输任务和恢复原因分层呈现；继续保持“连接设备”为唯一一级入口，管理和诊断放在次级入口，避免照搬企业后台导航。

## 明确不采纳的方向

- 不因为 RustDesk 的多平台范围而提前扩大 Windows 首发边界。
- 不把 MeshCentral 的企业设备管理、账号和无人值守权限引入个人远程控制默认路径。
- 不把 FreeRDP 的 RDP 兼容性目标当作 DeskLink 当前协议目标；DeskLink 只维护自己的端到端加密协议线。
