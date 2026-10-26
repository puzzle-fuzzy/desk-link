# Product

## Register

product

## Users

DeskLink serves one person connecting their own Windows and Mac devices. The primary interaction is a quick status check, followed by an occasional high-consequence action such as approving a new controller, disconnecting a live session, or revoking a trusted device.

## Product Purpose

DeskLink provides a dependable, private remote-desktop path between personal devices. Success means the owner can understand whether the host is available or controlled, recover from ordinary network disruption, and stop or revoke access immediately without learning networking or cryptographic terminology.

## 当前能力与阶段（2026-10-26）

用户已确认 Windows→Windows、macOS→Windows 的基础远控良好。Windows 是当前正式发布目标，现有构建处于公开测试阶段；产品能力、方向和本次验证以[当前能力矩阵](docs/current-capabilities.md)为准。Mac 控制侧的 IME、系统音频、剪贴板、文件和多屏工具分别记录源码接线、自动验证和实际行为验收，已合入的 Apple 代码不再统一写成尚未接通；被控 Mac 和 iOS 单列。Swift/Xcode、实际 WebView、输入注入、双机、拓扑、完整 Mac 功能、真实权限、恢复、性能、签名和安装仍保留各自未完成的验收范围。

应用版本、业务协议和工具链由 `python scripts/verify-windows-release.py --versions-only` 读取。第一波协议 13 软件候选 `a4c5b404d43794d646394923a481f23c9298c3ed` 已完整聚合各组稳定历史并通过 v3 的 25 个软件门禁，尚未合入仍为协议 12 的 main，也未完成正式发布验收。旧工程零 producer 与七处旧调用未迁移只属于 `5501aad`、`56f53b4`、`d6f153e` 的历史状态。GPU235 后续独立候选、文件协议14与LAN024后续生产接线不进入第一波。连接/共享两主入口与平台原生习惯继续保留，不因文档收口增加用户操作。

第二波独立GPU软件候选 `97ff7c08199cdad4511b0044ddf783d79e2295b1` 已重新通过软件与合成硬件检查，未改变第一波冻结提交。其纹理租借、surface编码和合成码流验证不等同真实桌面、设备恢复或性能承诺；完整范围见[GPU候选报告](docs/wave2-gpu-integration.md)。

## Application Account

DeskLink can be used immediately in local mode without an email account. An optional application account adds email verification, session management and account-device visibility; it does not grant remote-device access, replace host approval, or synchronize saved connections between devices. A single account may be used on multiple Windows, macOS, and iPhone installations, while each installation must pair with hosts independently. Logging out releases the current remote session, clears the installation's saved remote connection records, and removes its account session; users can re-enter local mode without an account.

## Brand Personality

Calm, precise, trustworthy. Windows and macOS share task vocabulary and a Chinese interface while preserving native platform behavior. Windows uses a clean, light desktop-tool interface with narrow navigation, a single green or teal accent, and clear hierarchy. Security information stays explicit and legible, while routine background operation stays unobtrusive. System dark mode and high-contrast settings remain supported.

## Windows Task Structure

“连接” and “共享” are the two primary entry points. Connection opens by default and centers on a device ID, an access password, and one primary connect action; recent connections support repeat use as a compact secondary list. Sharing centers on this device's ID, availability, and the controls needed to enable access and manage its password.

Approved devices, settings, diagnostics, about, and the optional account remain available with less visual weight. Each page has one task title and one primary focus. Keep copy to labels, necessary state, actionable recovery, and security decisions; avoid decorative panels, promotional descriptions, repeated headings, gradients, and visual effects. Exact colors, spacing, and component details are defined by the current Windows UI styles and [DESIGN.md](DESIGN.md).

## Anti-references

- Enterprise administration consoles with dense navigation and organizational features irrelevant to one person.
- Neon gaming or streaming overlays that make a security-sensitive host look recreational or noisy.
- Decorative security dashboards with meaningless gauges, glowing maps, or fear-driven warnings.
- Hidden background behavior, ambiguous connection states, and irreversible actions labeled only “OK” or “Yes.”

## Design Principles

1. Make connection and sharing immediate: foreground the current task and make it clear whether this desktop is available or controlled.
2. Make dangerous actions exact: identify the device and describe the consequence before approval, disconnect, or revocation.
3. Stay quiet when healthy: background availability belongs in the platform status area, not in a permanently intrusive window.
4. Explain recovery honestly: distinguish connecting, connected, retrying, expired, and permanently stopped states.
5. Preserve platform familiarity: share task structure and terminology while using native Windows and macOS behavior, keyboard access, system metrics, and standard interaction patterns.

## Accessibility & Inclusion

Target WCAG 2.2 AA-equivalent contrast and interaction clarity for custom-drawn content. Respect Windows DPI scaling and high-contrast themes plus macOS accessibility settings, keyboard navigation, screen-reader labels, and reduced-motion preferences. Never use color as the only status signal, and never require recognition of a truncated device fingerprint for a security decision.
