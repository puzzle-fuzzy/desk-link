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
3. 在两台真实 Windows 电脑完成同网、跨网、固定密码持续恢复、断线恢复、双屏/DPI、剪贴板、文件传输和长时间运行验收。
4. 保存脱敏诊断导出和人工验收记录；不要保存密码、私钥、完整设备身份或屏幕内容。

安装器会在写入程序前检查 Microsoft Edge WebView2 Evergreen Runtime。新电脑验收必须覆盖“已安装运行时正常进入界面”和“未安装运行时给出可行动提示且不留下半安装目录”两条路径。

## 2. 自动门禁

两台真实 Windows 的手工项目按 [Windows 双机验收记录](windows-two-pc-acceptance.md) 执行；该记录只描述当前候选协议线，不包含旧版本兼容路径。

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
python scripts/create-windows-acceptance-record.py --operator "待真实 Windows 验收"
python scripts/verify-windows-resilience.py --soak-seconds 10
python scripts/verify-managed-relay.py
python scripts/audit-managed-diagnostics.py
python -m unittest discover -s scripts/tests -p "test_*.py"
python scripts/check-windows-release-ready.py --manual-json dist/windows/windows-acceptance-record.json
python scripts/package-windows-candidate.py
```

必须检查：

- `dist/windows/windows-release-verification.json` 的 `passed` 为 `true`。
- `dist/windows/windows-installer-manifest.json` 的版本、x64、应用哈希、安装器哈希和 `passed` 正确。
- 两份清单的 `source_commit` 必须相同；正式签名构建的 `source_dirty` 必须为 `false`，并与 GitHub Actions 的 `GITHUB_SHA` 对齐。
- 正式发布前 `signed` 必须为 `true`；`signed: false` 只能用于本地候选包。
- `verify-managed-relay.py` 成功完成系统证书链和 QUIC 双向控制探测；`audit-managed-relay.py --expected-source-commit <HEAD>` 同时确认线上容器镜像的源码 revision 与候选提交一致。
- 诊断审计的公网 health、服务进程、定时任务和报告新鲜度均为通过。
- `windows-resilience-report.json` 必须来自干净的当前提交，并通过捕获、编码、主机恢复、电源恢复、本地加密媒体 soak，以及公网中继目录查询、Noise 握手、视频、输入和重连 E2E。soak 至少 10 秒，候选基线建议使用 300 秒。
- 捕获与编码验收还要确认首帧行为：静态桌面连接后应在 DXGI 无新帧时通过一次性 GDI 快照显示初始画面；如果系统权限、远程桌面驱动或安全桌面导致 10 秒内始终没有捕获首帧，或编码器持续不产出访问单元，主机必须进入可恢复的捕获/编码失败重连状态，而不是继续显示“已连接”并永久黑屏。
- 静态桌面不要求持续产生视频帧：验收至少确认一张完整关键帧可显示，并确认鼠标位置仍由独立光标通道持续更新；动态桌面再记录后续视频帧率。
- Windows CI 和签名候选 artifact 会同时上传 `windows-acceptance-record.json` 模板；真实验收只能在这份绑定当前安装包 SHA 的模板上填写，不能重新手写版本或哈希。
- 候选 ZIP 必须由 `package-windows-candidate.py` 生成；文件名包含当前源码提交短 SHA，且包内每份报告和安装器都经过同一候选绑定校验，不能使用无提交标识的旧 ZIP。

## 2.1 发布预检

在签名或创建 GitHub Release 前，先生成一份只读的统一预检报告：

```powershell
python scripts/check-windows-release-ready.py
```

报告默认写入 `dist/windows/windows-release-readiness.json`。它会把版本、当前提交 SHA、工作区洁净度、验证报告、安装包哈希、Windows-only 发布范围、签名、发布 tag、原生 Windows resilience 证据以及中继/诊断报告新鲜度放在一起检查；同时明确列出必须由发布负责人在两台真实 Windows 电脑上完成的验收项。当前命令默认只生成报告并返回 0，不会把未完成的手工验收伪装成通过。

用于 CI 或发布脚本时可启用严格模式：

```powershell
python scripts/check-windows-release-ready.py --strict
```

严格模式在仍有任何阻塞项时返回 1。为了避免把验收结果误绑定到另一份安装包，先基于当前候选生成记录模板：

### 2.2 Rust 依赖安全审计

发布冻结前运行：

```powershell
cargo audit
```

该命令覆盖整个 workspace 和所有条件平台，因此可能报告不会进入 Windows 正常依赖树的 GTK/zbus 依赖。当前中继已直接使用 `rustls::pki_types::pem::PemObject`，不再引入已停止维护的 `rustls-pemfile`；任何真正进入 Windows 或中继运行时依赖树的 RUSTSEC 告警都必须在签名发布前处理或明确记录风险，不得用忽略参数掩盖。

```powershell
python scripts/create-windows-acceptance-record.py --operator "release-team"
```

完成真实验收后，只修改模板中的 `checks` 和 `notes`，再传入 `--manual-json` 重新生成报告。每个标记为 `true` 的检查必须有非空 `notes`，说明设备范围、实际结果和异常处理。记录必须保留版本、来源提交 SHA、安装包 SHA-256、操作者和 UTC 时间：

```json
{
  "schema": 1,
  "version": "0.1.91",
  "source_commit": "<40-char-source-sha>",
  "installer": {
    "file_name": "DeskLinkSetup-0.1.91-x64.exe",
    "sha256": "<installer-sha256>"
  },
  "operator": "release-team",
  "recorded_at_utc": "2026-07-23T10:00:00Z",
  "checks": {
    "two_windows_acceptance": true,
    "long_soak_acceptance": true,
    "smartscreen_acceptance": true
  },
  "notes": {
    "two_windows_acceptance": "两台 Windows 已完成配对、视频、鼠标、键盘和双屏验收；异常：无。",
    "long_soak_acceptance": "连续运行 4 小时并完成断网恢复、剪贴板和文件传输；异常：无。",
    "smartscreen_acceptance": "全新 Windows 账户完成安装、升级、卸载和 SmartScreen 验收；异常：无。"
  }
}
```

然后运行 `python scripts/check-windows-release-ready.py --manual-json <path>`。预检会拒绝版本、提交 SHA 或安装包哈希不一致的记录。该文件不应包含密码、私钥、设备完整 ID 或屏幕内容。

## 3. 签名候选构建

正式构建必须在受控 Windows runner 或受控签名机完成：

```powershell
python scripts/build-windows-installer.py --require-signing
```

GitHub Actions 使用 `Windows Signed Candidate` 工作流。该工作流只生成并上传不可变的签名候选 artifact，不直接创建 GitHub Release。PFX 和密码只放在 GitHub Secrets：

- `WINDOWS_SIGNING_PFX_BASE64`
- `WINDOWS_SIGNING_PFX_PASSWORD`

签名构建必须同时满足：

- 主程序和最终安装器都包含有效 Authenticode 签名。
- 证书用途包含 Code Signing，证书在有效期内且链完整。
- 签名使用 SHA-256，并包含 RFC 3161 时间戳。
- 发布清单中的 `signed` 为 `true`，哈希与最终文件一致。
- 不在日志、artifact 名称或仓库中暴露 PFX、密码、私钥或临时签名文件。

记录该工作流的 run ID。后续发布工作流会按 `DeskLink-Windows-signed-<run-id>` 下载同一份安装器，不重新构建或重新签名。

## 4. 发布证据交接

正式发布前，将最终签名安装器的验收和运维报告提交到仅包含证据的分支或提交，并使用不可变的 40 位提交 SHA。目录约定为：

```text
release-evidence/v0.1.91/windows-acceptance-record.json
release-evidence/v0.1.91/windows-resilience-report.json
release-evidence/v0.1.91/managed-relay-verification.json
release-evidence/v0.1.91/managed-diagnostics-audit.json
```

证据提交不得修改产品源码、安装包或私钥；人工验收暂未完成时，不创建这些“通过”证据，也不要运行正式发布工作流。

## 5. 创建 Release

签名候选构建、人工验收和运维证据都完成后，才创建与候选源码提交一致的 annotated tag，例如：

```powershell
git tag -a v0.1.91 -m "DeskLink Windows 0.1.91"
git push origin v0.1.91
```

在该 tag 上手动运行 `Windows Publish Release`，输入签名候选 workflow run ID 和证据提交 SHA。它会下载同一份签名 artifact，导入三份 source-bound 证据，执行 `check-windows-release-ready.py --strict`，并由 `publish-windows-release.py` 进行第二次 readiness 校验；未签名、版本不匹配或任何 P0/P1 门禁未完成时不得上传 GitHub Release。发布内容至少包括：

- `DeskLinkSetup-0.1.91-x64.exe`
- `windows-installer-manifest.json`
- `windows-release-verification.json`
- `windows-release-readiness.json`
- SHA-256 和签名状态
- 已知限制与回滚说明

发布脚本还会强制要求 GitHub 提供有效的 GITHUB_SHA，并确认签名物料与当前 tag 指向同一提交。

发布脚本在调用 GitHub CLI 上传前，会对即将上传的最终安装器再次运行
`sign-windows-artifact.py --verify-only`。清单中的 `signed: true` 不是签名证明；
最终 PE 文件的 Authenticode 验证失败时，上传会立即终止。

## 6. 回滚

发现连接、权限、数据损坏或安全问题时：

1. 立即停止推广当前 Release，并在发布页标记为 withdrawn，不删除审计记录。
2. 恢复上一份已签名且人工验收通过的安装器及其清单。
3. 不回滚用户 `%LOCALAPPDATA%\DeskLink` 数据目录，除非确认数据格式或密钥存储损坏；需要迁移时必须提供明确备份和恢复步骤。
4. 如果问题来自中继，先切换到已验证的 relay 配置，再保留故障节点日志用于分析。
5. 将版本、提交 SHA、安装器 SHA-256、影响范围和修复版本写入变更记录。

## 7. 诊断与隐私

- 诊断必须由用户主动开启，默认关闭。
- 客户端只上传脱敏事件、单向会话关联和有界性能计数。
- 服务端拒绝密码、私钥、完整设备身份、长十六进制密钥、屏幕内容和文件完整路径。
- 云端只保留约定期限内的诊断数据；发布排查结束后清理临时导出。
- 对外反馈优先提供报告 ID、时间窗口、版本和路径，不要求用户发送原始日志中的秘密字段。

## 8. 发布后观察

发布后首个观察窗口重点关注：

- 中继 TLS/QUIC 成功率和连接恢复失败。
- 主机服务停止、重复会话和审批超时。
- 视频关键帧恢复、渲染积压和 DirectLan 回落比例。
- 文件接收失败、队列恢复和剪贴板确认超时。
- 安装器启动、升级、卸载和 SmartScreen 反馈。

4K、全量 P2P、macOS 和其他后续能力必须以独立版本计划推进，不得在发布修复中偷偷扩大当前协议或权限边界。
