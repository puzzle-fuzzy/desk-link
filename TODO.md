# DeskLink 上线 TODO

> 目标：先把 Windows 两台电脑的远程控制做成可发布的稳定版本，再扩展 4K 和公网 P2P。
> 当前判断（2026-07-23）：核心功能约 90% 实现；正式上线完备度约 65%～70%。
> 规则：`[x]` 只表示代码或自动验证已完成；需要两台真实 Windows 电脑的项目必须由人工验收后再勾选。

## 0. 发布基线与工作区（P1）

- [ ] 整理当前工作区变更，确认所有 DirectLan、视频质量、UI 和诊断代码都属于本次发布范围。
- [ ] 更新版本号、变更日志和发布说明，形成唯一的发布提交。
- [ ] 在干净 checkout 上通过 Rust、Bun、安装包和发布校验。
- [ ] 创建 `v0.1.91`（或下一版本）发布 tag，并保留可回滚提交。

## 1. 云端诊断可用性（P1）

- [x] 将 `desklink-diagnostics` 的 `/desklink-diagnostics/health` 和 `/desklink-diagnostics/v1/batches` 路由加载到生产 Nginx。
- [x] reload Nginx 后验证公网 health、限流和批量上报接口。
- [x] 运行 `scripts/audit-managed-diagnostics.py`，保存审计报告。
- [x] 在 Windows 客户端完成一次脱敏诊断上传，确认云端可检索且不包含密码、密钥、屏幕内容。

## 2. 局域网直连闭环（P1）

- [x] 让 `ControllerRuntime` 的 DirectVideoPath 状态机真正驱动候选探测和激活，而不是只根据已建立连接推断路径。
- [x] 增加认证探测、状态机激活和视频 datagram 传递的 transport loopback 测试；探测本身不会放行 4K。
- [x] 覆盖控制端对端先发起探测时的并发窗口，避免 Noise 会话锁导致假超时。
- [x] 被控端增加入站 DirectLan 探测监听，与已有出站连接互为兜底；Rust Windows 包测试、Clippy 和认证 datagram 回环回归测试已通过。
- [ ] 补充 HostRuntime ↔ ControllerRuntime 的 DirectLan 双机/回环 E2E：成功、超时、候选过期、认证失败、主动关闭。
- [x] 视频 datagram 发送失败时立即撤销 DirectLan 句柄并重发同一包到中继；自动回落测试通过。
- [ ] 验证直连失败后视频自动回落中继，且控制、审批、剪贴板、文件仍保持中继通道。
- [x] 增加直连 RTT、丢包和当前路径诊断字段；回落后仍保留最近探测质量和回落原因，避免用户看到“已直连”但实际仍走中继。

## 3. Windows 双机验收（P1，需人工）

- [ ] 同一局域网：两台 Windows 配对、审批、视频、鼠标、键盘、多屏切换。
- [ ] 不同网络：通过公网中继完成同样流程。
- [ ] 断网/切 Wi-Fi/中继重启后自动恢复，不出现重复会话或“主机服务已停止”。
- [ ] 睡眠唤醒、锁屏、窗口最小化/恢复、DPI 不同和双屏场景。
- [ ] 剪贴板、单文件/多文件传输、暂停/恢复、取消和失败重试。
- [ ] 连续运行至少 4 小时，记录 CPU、GPU、内存、帧率、延迟和错误日志。

## 4. 安装与信任（P1）

- [ ] 获取 Windows Authenticode 代码签名证书并安全保存 PFX。
- [ ] 配置 GitHub Actions secrets：`WINDOWS_SIGNING_PFX_BASE64`、`WINDOWS_SIGNING_PFX_PASSWORD`。
- [ ] 运行签名构建，确认安装包和主程序签名有效、证书链完整。
- [ ] 在全新 Windows 账户验证 SmartScreen、安装、升级、卸载和数据保留策略。
- [ ] 生成带 SHA-256、签名状态和构建提交的发布清单。

## 5. 产品文档与 UI 收口（P2）

- [x] 清理 README 和历史版本说明，移除“直连尚未启用”等过时描述。
- [x] 明确产品能力边界：当前最高 2560×1440；4K 和公网 NAT 穿透暂不承诺。
- [ ] 统一 `DESIGN.md` 与 Windows UI 样式 token，删除多轮迭代产生的冲突规则。
- [ ] 手动验收键盘导航、焦点可见性、高对比度、WebView2 缩放和中文文案溢出。
- [ ] 将发布前检查、服务器回滚和日志脱敏流程写入运行手册。

## 6. 后续增强（不阻塞首个 Windows 正式版）

- [ ] 4K 原生编码能力评估、硬件矩阵和自适应码率策略。
- [ ] 公网 STUN/ICE/TURN 路径评估，决定是否增加真正的跨网 P2P 视频通道。
- [ ] UAC/安全桌面、语音输入、虚拟桌面等是否纳入下一版本范围。
- [ ] macOS VideoToolbox 适配（仅当恢复跨平台发布目标时）。

## 下一阶段执行顺序（发布冻结前）

1. **收口当前直连诊断与回落改动**：重新跑 workspace 门禁，提交并推送当前 8 个文件；发布说明只描述已验证能力，不扩展 4K 承诺。
2. **运行时 E2E 准备**：把 HostRuntime ↔ ControllerRuntime 的候选交换、超时、候选过期、认证失败和主动关闭整理成可重复的测试夹具；真实桌面采集仍单独走人工验收。
3. **双机验收包**：使用现有 `verify-windows-resilience.py` 和安装包清单，记录同网直连、跨网中继、断线恢复、双屏/DPI、剪贴板和文件传输结果。
4. **信任与安装**：用户提供 Authenticode 证书后执行签名构建、SmartScreen 和全新账户安装升级验收；没有证书时保持候选版，不伪装成正式发布。
5. **发布冻结**：在干净 checkout 重跑所有门禁，生成 SHA-256/签名清单，最后创建 `v0.1.91` tag；4K、公网 P2P 和 macOS 继续留在后续版本。

## 自动门禁

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bun test
bun run build
python scripts/verify-windows-release.py
python scripts/build-windows-installer.py
python scripts/verify-managed-relay.py
python scripts/audit-managed-diagnostics.py
```

### 最近一次自动门禁（2026-07-23）

- [x] Rust workspace：`cargo fmt --all -- --check`、`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`。
- [x] Windows 发布验证：`python scripts/verify-windows-release.py`（152 个前端测试通过，TypeScript/Vite/Rust release 构建通过）。
- [x] Windows 安装包构建：`python scripts/build-windows-installer.py`（安装包清单生成，当前 `signed: false`）。
- [x] 主机 DirectLan 接入回环：`cargo test -p desklink-windows --lib runtime::direct_video_tests::host_acceptor_keeps_authenticated_direct_datagram_connection`。
- [x] 直连质量可观测性：记录当前路径、最近 RTT、丢包基点和回落原因；`cargo clippy --workspace --all-targets -- -D warnings` 与 Windows UI 控制器测试通过。
- [x] 中继探测：`python scripts/verify-managed-relay.py`（双向控制探测通过，约 303 ms）。
- [x] 云诊断审计：`python scripts/audit-managed-diagnostics.py`（公网 health、服务、定时器和报告新鲜度通过）。
- [x] 变更检查：`git diff --check`。

## 当前已知事实

- 中继实况探测已通过：`101.35.246.159:4433`。
- 本地诊断服务、定时器、公网诊断 health 和 Windows 脱敏 HTTPS 上报已通过审计；服务器诊断发布为 `d3365a49f138`，最近一次 Nginx 配置备份在 `/etc/nginx/conf.d/p2p.yxswy.com.conf.bak-desklink-1784743477`。
- 当前安装包 `dist/windows/DeskLinkSetup-0.1.91-x64.exe` 未签名。
- 候选版本变更边界已整理到 [CHANGELOG.md](CHANGELOG.md)，但尚未形成干净的唯一发布提交。
- 当前工作区包含本轮直连诊断字段的未提交修改，尚无本地 `v*` 发布 tag。
