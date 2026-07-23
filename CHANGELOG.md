# DeskLink 更新记录

## 0.1.91（候选版本，尚未正式发布）

### 已完成

- Windows 控制端与主机端的视频链路增加认证 DirectLan 探测；探测失败时继续使用公网中继。
- DirectLan 状态机、控制端主动探测窗口、主机端入站探测监听和 datagram 回环回归测试已接入。
- H.264 Main/High Profile 协商、视频质量自适应和渲染诊断指标已纳入 Windows 构建门禁。
- 云端脱敏诊断入口已加载到生产 Nginx；Windows HTTPS 脱敏批次上报和服务端字段白名单已验证。
- Windows UI、Rust workspace、在线中继探测和安装包构建门禁通过。
- Windows UI 统一为单一 editorial 令牌：暖纸背景、蓝图蓝操作色、方角分组和 Cascadia Mono 元数据；删除重复的旧圆角/珊瑚色覆盖层。
- 发布边界收敛为 Windows 10/11 x64；macOS 源码保留为暂存研究代码，不进入本候选版本的构建、测试和发布承诺。
- 修复 DirectLan 被拒绝后未保留回落原因的问题，并增加 FFI 中继回落回归测试，确认视频回落后控制输入和关键帧请求仍可用。

### 当前明确限制

- 这是候选构建，不是正式发布版本；当前安装包和主程序仍未进行 Authenticode 签名。
- 两台真实 Windows 电脑的同网直连、跨网中继、断线恢复、多屏/DPI、剪贴板和文件传输仍需人工验收。
- DirectLan 当前只负责视频 datagram；控制、审批、剪贴板和文件传输仍走端到端加密的中继通道。
- 开发协议只接受当前带 participant identity 的 relay join、带协议版本的目录查询和目录登记；旧开发信封不再作为兼容路径。
- 安装器只维护单一 `DeskLink.exe` 入口，旧的独立 host 文件和旧设备记录格式不会再参与启动或恢复。
- 4K 原生编码和公网 NAT 穿透 P2P 尚未开放或承诺，当前以 2560×1440 为清晰度上限。

### 验证记录（2026-07-23）

- `cargo fmt --all -- --check`
- `cargo test --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `python scripts/verify-windows-release.py`
- `python scripts/build-windows-installer.py`
- `python scripts/verify-managed-relay.py`
- `python scripts/audit-managed-diagnostics.py`
