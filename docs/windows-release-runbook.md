# DeskLink Windows 发布运行手册

本文只覆盖 DeskLink 第一个 Windows 正式版本。真实双机验收由发布负责人在两台 Windows 电脑上完成；本手册不把本地单元测试或中继探针当作双机验收替代品。

## 发布边界

- 目标平台：Windows 10/11 x64。
- 当前候选版本：`0.1.91`。
- 发布入口：单一 `DeskLink.exe` 和单一 `DeskLinkSetup-<version>-x64.exe`。
- 视频：最高约 2560×1440；DirectLan 只优化视频数据面，控制、审批、剪贴板和文件继续使用端到端加密中继。
- 不属于本次发布：macOS、4K、全量公网 P2P、UAC 安全桌面、语音对讲、虚拟桌面和后台静默更新。

## 1. 发布前冻结

1. 确认工作区干净，当前提交已推送到 `main`，并记录提交 SHA。
2. 确认 [TODO.md](../TODO.md) 中的真实双机验收和签名项已经有负责人；未完成时只能标记为候选版。
3. 在两台真实 Windows 电脑完成同网、跨网、断线恢复、双屏/DPI、剪贴板、文件传输和长时间运行验收。
4. 保存脱敏诊断导出和人工验收记录；不要保存密码、私钥、完整设备身份或屏幕内容。

## 2. 自动门禁

在 Windows x64 工作目录执行：

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace

cd apps/windows-ui
bun install --frozen-lockfile
bun test
bun run build
cd ../..

python scripts/verify-windows-release.py
python scripts/build-windows-installer.py
python scripts/verify-managed-relay.py
python scripts/audit-managed-diagnostics.py
python -m unittest discover -s scripts/tests -p "test_*.py"
```

必须检查：

- `dist/windows/windows-release-verification.json` 的 `passed` 为 `true`。
- `dist/windows/windows-installer-manifest.json` 的版本、x64、应用哈希、安装器哈希和 `passed` 正确。
- 正式发布前 `signed` 必须为 `true`；`signed: false` 只能用于本地候选包。
- `verify-managed-relay.py` 成功完成系统证书链和 QUIC 双向控制探测。
- 诊断审计的公网 health、服务进程、定时任务和报告新鲜度均为通过。

## 3. 签名构建

正式构建必须在受控 Windows runner 或受控签名机完成：

```powershell
python scripts/build-windows-installer.py --require-signing
```

GitHub Actions 使用 `Windows Signed Release` 工作流。PFX 和密码只放在 GitHub Secrets：

- `WINDOWS_SIGNING_PFX_BASE64`
- `WINDOWS_SIGNING_PFX_PASSWORD`

签名构建必须同时满足：

- 主程序和最终安装器都包含有效 Authenticode 签名。
- 证书用途包含 Code Signing，证书在有效期内且链完整。
- 签名使用 SHA-256，并包含 RFC 3161 时间戳。
- 发布清单中的 `signed` 为 `true`，哈希与最终文件一致。
- 不在日志、artifact 名称或仓库中暴露 PFX、密码、私钥或临时签名文件。

## 4. 创建 Release

只有签名构建和人工验收都完成后，才创建与版本一致的标签，例如：

```powershell
git tag -a v0.1.91 -m "DeskLink Windows 0.1.91"
git push origin v0.1.91
```

`Windows Signed Release` 会再次执行强制签名门禁；未签名或版本不匹配时不得上传 GitHub Release。发布内容至少包括：

- `DeskLinkSetup-0.1.91-x64.exe`
- `windows-installer-manifest.json`
- `windows-release-verification.json`
- SHA-256 和签名状态
- 已知限制与回滚说明

## 5. 回滚

发现连接、权限、数据损坏或安全问题时：

1. 立即停止推广当前 Release，并在发布页标记为 withdrawn，不删除审计记录。
2. 恢复上一份已签名且人工验收通过的安装器及其清单。
3. 不回滚用户 `%LOCALAPPDATA%\DeskLink` 数据目录，除非确认数据格式或密钥存储损坏；需要迁移时必须提供明确备份和恢复步骤。
4. 如果问题来自中继，先切换到已验证的 relay 配置，再保留故障节点日志用于分析。
5. 将版本、提交 SHA、安装器 SHA-256、影响范围和修复版本写入变更记录。

## 6. 诊断与隐私

- 诊断必须由用户主动开启，默认关闭。
- 客户端只上传脱敏事件、单向会话关联和有界性能计数。
- 服务端拒绝密码、私钥、完整设备身份、长十六进制密钥、屏幕内容和文件完整路径。
- 云端只保留约定期限内的诊断数据；发布排查结束后清理临时导出。
- 对外反馈优先提供报告 ID、时间窗口、版本和路径，不要求用户发送原始日志中的秘密字段。

## 7. 发布后观察

发布后首个观察窗口重点关注：

- 中继 TLS/QUIC 成功率和连接恢复失败。
- 主机服务停止、重复会话和审批超时。
- 视频关键帧恢复、渲染积压和 DirectLan 回落比例。
- 文件接收失败、队列恢复和剪贴板确认超时。
- 安装器启动、升级、卸载和 SmartScreen 反馈。

4K、全量 P2P、macOS 和其他后续能力必须以独立版本计划推进，不得在发布修复中偷偷扩大当前协议或权限边界。
