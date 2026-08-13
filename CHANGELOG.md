# DeskLink 更新记录

## 0.1.91（候选版本，尚未正式发布）

### 已完成

- Windows 控制端与主机端的视频链路增加认证 DirectLan 探测；探测失败时继续使用公网中继。
- DirectLan 状态机、控制端主动探测窗口、主机端入站探测监听和 datagram 回环回归测试已接入。
- H.264 Main/High Profile 协商、视频质量自适应和渲染诊断指标已纳入 Windows 构建门禁。
- 云端脱敏诊断入口已加载到生产 Nginx；Windows HTTPS 脱敏批次上报和服务端字段白名单已验证。
- Windows UI、Rust workspace、在线中继探测和安装包构建门禁通过。
- Windows UI 收敛为连接优先的产品化界面：白色背景、蓝色主操作、圆角控件和响应式字号；共享此设备、已批准设备、设置 / 诊断与关于入口统一收进“更多”菜单，保留原有连接与安全操作。
- Windows UI 第二轮收口：连接设备作为唯一一级导航，已保存设备改为可伸缩列表；共享、批准设备、设置 / 诊断统一收进“更多”菜单，支持点击空白处关闭，所有层级使用边框和背景变化表达，不依赖卡片阴影。
- Windows 首发改为本机优先：未登录账号也可以直接使用远程控制，主机服务不会因账号服务不可用而停止；账号登录、注册、找回密码和退出仍保留在“更多”入口。
- 本机模式偏好按当前 WebView 保存，退出账号继续清除本机保存的远程连接记录；账号服务 CI 已纳入测试与类型检查。
- Windows UI 采用 RemoteFlow 设计系统：近白色画布、4px 基线、v-sans / 中文系统回退、#2563eb 主操作色、8px 组件圆角和低对比边框层级。
- 发布边界收敛为 Windows 10/11 x64；macOS 源码保留为暂存研究代码，不进入本候选版本的构建、测试和发布承诺。
- 修复 DirectLan 被拒绝后未保留回落原因的问题，并增加 FFI 中继回落回归测试，确认视频回落后控制输入和关键帧请求仍可用。
- 远程画面质量状态改为基于最近 10 秒滚动窗口，避免一次历史卡顿长期污染当前提示；检测到明显卡顿时可在状态徽标中一键切换到流畅画质，切换期间会锁定操作并在确认或超时后恢复状态。
- 修复连接操作返回快照与实时状态通道乱序时的状态回退：如果连接期间已经收到更新的“已连接/恢复中”状态，界面会保留实时运行状态，同时合并最新的设备与恢复信息。
- 直连视频增加 5 秒无数据 watchdog：探测成功但数据被防火墙静默丢弃时，自动关闭失效直连并回落中继，诊断记录为 `datagramSilence`，避免连接在线但画面永久黑屏。
- 主机生命周期增加代数隔离：重启、停止或重新进入配对时，旧配对任务不能覆盖新主机状态或重复启动 worker；配对完成仅在当前生命周期仍有效时自动恢复普通主机。
- Windows 发布预检现在强制消费与当前源码提交绑定的原生 resilience 报告；捕获、编码、主机恢复、电源恢复和最短 10 秒媒体 soak 未通过时，不允许进入正式发布门禁。
- 停止或退出主机服务时强制清理配对状态；即使配对 worker 超时被 abort，也不会把下一次启动误判为仍在配对。

### 当前明确限制

- 这是候选构建，不是正式发布版本；当前安装包和主程序仍未进行 Authenticode 签名。
- 两台真实 Windows 电脑的同网直连、跨网中继、断线恢复、多屏/DPI、剪贴板和文件传输仍需人工验收。
- DirectLan 当前只负责视频 datagram；控制、审批、剪贴板和文件传输仍走端到端加密的中继通道。
- 开发协议只接受当前带 participant identity 的 relay join、带协议版本的目录查询和目录登记；旧开发信封不再作为兼容路径。
- 安装器只维护单一 `DeskLink.exe` 入口，旧的独立 host 文件和旧设备记录格式不会再参与启动或恢复。
- 4K 原生编码和公网 NAT 穿透 P2P 尚未开放或承诺，当前以 2560×1440 为清晰度上限。

### 最新验证记录（2026-08-13）

- 本轮稳定性修复已推送 `main`（当前候选提交为 `2008283`）；本次 UI 信息架构收口后的候选物料已重新生成。前端 164 项测试、Vite 构建、Rust fmt/Clippy、workspace 测试、Windows release verification 和安装包构建全部通过。
- 当前候选安装包仍未签名；安装器来源、大小和 SHA-256 以同批生成的 `windows-installer-manifest.json` 与 `windows-release-readiness.json` 为准。
- `windows-release-readiness.json` 已作为构建证据生成；当前为 `ready: false`，签名、发布 tag、运维证据和真实 Windows 验收完成前不会创建正式 Release。
- 发布脚本与签名工作流均已增加严格 readiness 校验，避免只凭安装器构建成功就发布。

### 先前验证记录（2026-07-28）

- `cargo fmt --all -- --check`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `python scripts/verify-windows-release.py`
- `python scripts/build-windows-installer.py`
- `python scripts/verify-managed-relay.py`
- `python scripts/audit-managed-diagnostics.py`
- Windows UI 参考 RemoteFlow 控制台重新排版：恢复原生 Windows 标题栏，移除自绘窗口控制；主界面改为左侧导航栏、工作区上下文栏和连接设备工作区，保留现有连接、批准、共享与诊断交互。
- Windows UI 本机模式单元测试与完整前端门禁通过（155 项测试）；账号服务测试 18 项、诊断服务测试/类型检查、Rust workspace 与 Clippy 通过。
- 候选安装包来源提交为 `e6f8e6c`，未签名，SHA-256：`8d8112d7ae7943bcc2d786f43d5dc7f2a51bb3652ca801d8e3c8a9033ea57ddc`。
