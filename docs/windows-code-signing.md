# Windows 代码签名

DeskLink 的发布构建会按正确顺序处理应用与安装器两个可执行文件：

1. 构建并签名现代入口 `DeskLink.exe`；
2. 把已签名应用嵌入单文件安装器；
3. 构建并签名最终 `DeskLinkSetup-<version>-x64.exe`；
4. 对两个签名执行 Authenticode 发布策略和时间戳验证。

没有配置签名身份时，`python scripts/build-windows-installer.py` 仍会生成明确标记为 `unsigned` 的本地测试包。正式发布必须执行 `python scripts/build-windows-installer.py --require-signing`；也可设置 `DESKLINK_REQUIRE_SIGNING=1`。门禁会在耗时构建前拒绝缺少签名身份的任务。

## 方案 A：传统公有代码签名证书

这是直接分发 EXE 安装器时覆盖地区最广的方案。向受 Windows 信任的证书颁发机构申请组织验证（OV）代码签名证书，并按供应商要求把私钥保存在 USB 硬件令牌或云 HSM 中。让证书出现在当前用户的 Windows `My` 证书存储后，读取 SHA-1 指纹：

```powershell
Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert |
  Select-Object Subject, Thumbprint, NotAfter
```

构建前配置环境变量；时间戳地址优先使用证书供应商给出的 RFC 3161 服务：

```powershell
$env:DESKLINK_SIGN_CERT_SHA1 = "40位证书SHA-1指纹"
$env:DESKLINK_TIMESTAMP_URL = "http://timestamp.digicert.com"
python scripts/build-windows-installer.py
```

私钥或 PIN 不进入仓库、环境变量或构建参数。硬件令牌可能在签名时弹出供应商的 PIN 窗口；云 HSM 则由供应商客户端完成认证。

## 方案 B：Microsoft Artifact Signing

适用地区和身份通过 Microsoft 验证后，可创建 Public Trust certificate profile，安装 Artifact Signing Client Tools，并准备不含密钥的 metadata JSON：

```json
{
  "Endpoint": "https://<account>.<region>.codesigning.azure.net/",
  "CodeSigningAccountName": "<account>",
  "CertificateProfileName": "<profile>",
  "CorrelationId": "DeskLink-release"
}
```

配置由 Microsoft 客户端提供的 dlib 和 metadata 文件：

```powershell
$env:DESKLINK_ARTIFACT_SIGNING_DLIB = "C:\path\Azure.CodeSigning.Dlib.dll"
$env:DESKLINK_ARTIFACT_SIGNING_METADATA = "C:\secure\desklink-signing-metadata.json"
python scripts/build-windows-installer.py
```

Azure 身份权限由 Artifact Signing 的 `Certificate Profile Signer` 角色控制。metadata 可以放在仓库外；访问令牌和身份凭据不应提交到 Git。

Microsoft 官方要求 SignTool 至少来自 Windows SDK 10.0.2261.755，并建议使用 Artifact Signing 自己的 RFC 3161 时间戳服务。DeskLink 的脚本已固定 SHA-256 文件摘要和 SHA-256 时间戳摘要，并在 Artifact Signing 模式自动切换到该时间戳服务。

## GitHub 可信签名发布

仓库的 `.github/workflows/windows-signed-release.yml` 可手动运行，也会在推送 `v*` 标签时运行。工作流在临时 Windows runner 中导入 PFX，验证证书同时满足以下条件后才构建：

- 当前处于有效期；
- 包含私钥；
- 包含 Code Signing EKU `1.3.6.1.5.5.7.3.3`；
- PFX 中恰好有一个符合条件的签名证书。

获得受信任 CA 签发且允许 CI 使用的 PFX 后，在已登录 `gh` 的本机执行：

```powershell
python scripts/configure-github-windows-signing.py C:\secure\desklink-code-signing.pfx
```

脚本会隐藏密码输入，通过标准输入写入 `WINDOWS_SIGNING_PFX_BASE64` 与 `WINDOWS_SIGNING_PFX_PASSWORD` 两个 GitHub Secrets，不会把 PFX、密码或 base64 私钥材料打印到终端或写进仓库。之后在 GitHub Actions 手动运行 `Windows Signed Release`，下载的 artifact 才是可分发候选包。

如果证书私钥只能保存在 USB 硬件令牌或供应商云 HSM，不能导出 PFX，就不要把它迁移到 GitHub Secrets。应在受控 Windows 签名机上使用方案 A，或使用方案 B 的 Microsoft 托管身份，再执行同一个强制签名构建命令。

## 单文件验证与故障定位

只签一个已构建文件：

```powershell
python scripts/sign-windows-artifact.py dist\windows\DeskLinkSetup-0.1.1-x64.exe
```

只验证已有签名和 RFC 3161 时间戳：

```powershell
python scripts/sign-windows-artifact.py --verify-only dist\windows\DeskLinkSetup-0.1.1-x64.exe
```

脚本默认查找最新 Windows SDK x64 `signtool.exe`；如果 SDK 在自定义位置，可设置 `DESKLINK_SIGNTOOL`。任何签名或验证警告/失败都会让构建失败，避免误发布未签名或未加时间戳的包。SignTool 使用 `/pa /all /v /tw` 复核发布策略、全部签名和时间戳。

自签名证书只适合开发机或已通过组策略分发根证书的内部环境，不适合公开下载。证书续期时替换证书存储中的证书并更新指纹即可；已正确加 RFC 3161 时间戳的旧版本仍可在签名证书过期后验证。
