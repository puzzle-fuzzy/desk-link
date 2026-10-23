# Windows 客户端更新

从 0.1.92 开始，客户端在启动时后台检查更新，有新版本时显示“更新并重启”。用户点击后下载并验证安装包，安装成功重开应用。下载期间可以取消；检查失败不打断连接或共享，设置页可重试。更新阶段只刷新独立区域，避免重建远程画面或正在输入的表单。

0.1.91 及以前只有版本检查和下载页面入口，因此需要先手动覆盖安装一次。更新安装沿用 `%LOCALAPPDATA%\Programs\DeskLink`，不清理 `%LOCALAPPDATA%\DeskLink`，也不改变登录后自动启动选择。点击更新会在安装时结束当前连接。

## 更新来源与校验

生产客户端固定读取 `puzzle-fuzzy/desk-link` 的 GitHub 最新稳定 Release。前端不能传入下载地址、文件路径或目标版本；原生代码重新检查版本，拒绝降级、草稿和预发布版本。需要同一个 Release 中的三个文件：

- `DeskLinkSetup-<version>-x64.exe`
- `windows-installer-manifest.json`
- `windows-installer-manifest.json.sig`

客户端用 `apps/windows-ui/src-tauri/update-public-key.hex` 中的公开密钥，验证 `b"desklink-windows-update-manifest-v1\0" + 清单原始字节` 的 Ed25519 签名；`.sig` 为 128 位小写十六进制签名，可无换行或使用 LF / CRLF。验签后核对清单版本、Windows x64 target、`update_mode: perUserV1`、文件名、大小与 SHA-256，再对实际下载文件核对大小和摘要。不能用远程 `signed: true` 布尔值替代验签。

独立更新签名用于授权客户端接受的更新，与 Windows Authenticode 代码签名分开。测试构建可以缺少 Authenticode，但必须由独立发布密钥授权才可作为客户端更新；正式 Release 仍须满足现有 Authenticode、干净源码和人工验收门禁。

## 发布密钥与本地构建

安装签名脚本依赖：

```powershell
python -m pip install -r scripts/requirements-windows-release.txt
```

更新公钥已纳入源码。当前发布机的私钥仅保存在仓库外 `%LOCALAPPDATA%\DeskLinkRelease\update-signing-key.dpapi`，使用当前 Windows 用户 DPAPI 保护。构建日志、安装包与仓库均不包含私钥；普通用户安装应用不需要该文件。不要重新生成公钥来替代丢失的私钥，已安装客户端会拒绝新的密钥。密钥迁移、备份和轮换必须单独安排；DPAPI 文件不能直接供另一个 Windows 用户解锁。

在最初尚无任何公钥和私钥时，`python scripts/windows_update_signing.py init` 可创建密钥，存在时拒绝覆盖。发布机默认读取上述位置；也可通过 `DESKLINK_UPDATE_SIGNING_KEY` 指定仓库外的 DPAPI 文件路径，变量不包含密钥内容。

```powershell
# 使用项目固定的 Bun 1.3.14
python scripts/build-windows-installer.py --require-update-signing
python scripts/windows_update_signing.py verify dist/windows/windows-installer-manifest.json
```

构建脚本在耗时构建前验证密钥与内置公钥匹配，最终安装器生成后才签署清单。`--require-signing` 同时要求 Authenticode 和更新签名。未配置更新密钥的本地构建可以生成测试安装包，但会移除旧 `.sig`，不会被客户端接受为更新。签名后修改清单（即使只变空白）也必须重新签名。

正式发布脚本会再次验证安装器 Authenticode 与清单独立签名，并上传三个文件及发布验证记录。不要把签名私钥上传为 Release asset。当前工作未修改 GitHub Actions；在其他发布机运行前需单独配置密钥和依赖。

## 公开测试发布

0.1.93 提供单独的公开测试发布入口，不改变正式发布的 Authenticode 与人工验收门禁。使用干净的隔离源码分支，提交精确 Windows 发布源码并创建对应 tag 后执行：

```powershell
python scripts/build-windows-installer.py --public-test
python scripts/publish-windows-public-test.py validate --tag v0.1.93
python scripts/publish-windows-public-test.py draft --tag v0.1.93
python scripts/publish-windows-public-test.py publish --tag v0.1.93
```

构建清单明确记录 `release_channel: publicTest`、`signed: false` 和干净的源码提交；独立签名不可缺少。发布脚本先创建草稿，下载并逐一核对四个附件，最终发布前再次验证源码、tag、公钥签名与精确安装包。公开的验证报告采用字段白名单，不包含本机绝对路径、用户数据或诊断日志，也不宣称正式 readiness。

0.1.92 客户端固定读取 GitHub `releases/latest`，因此公开测试版本使用三段数字版本、非 draft、非 prerelease 的 Release，并在标题和说明明确注明未做 Authenticode 的测试性质。仓库必须已经公开，发布工具不会自行改变仓库可见性。正式上线仍使用 `publish-windows-release.py`。

## 验收边界

源码和隔离测试覆盖签名、文件篡改、版本筛选、下载中断、取消、重复操作、安装事务和设置保留。公共最新 Release 当前尚无可用版本；自动检查不会凭空生成更新。发布第一个稳定版本后，还需在两台实际安装的 Windows 电脑验证“发现新版→下载→安装→重启→重新连接”、中文输入及固定密码继续可用。不要通过开发目录中的可执行文件覆盖当前用户的真实安装来模拟验收。

2026-10-23 本地验证：246 项前端测试、90 项桌面端测试、19 项安装器测试通过；Clippy、rustfmt 和生产前端构建通过。实际 Chrome 的隔离 IPC 页面检查了宽屏、480×640、账号页、下载取消及键盘重试，进度更新保留原输入节点和焦点；所有预览使用合成设备数据。原生测试使用临时 HTTP 服务和无害的临时 Windows PE，未更新当前用户安装。构建完成后需额外显式运行 `updates::tests::actual_built_manifest_verifies_with_embedded_public_key`，验证精确安装包、签名与客户端内置公钥一致。
