# Windows 代码签名

DeskLink 的发布构建会按正确顺序处理应用与安装器两个可执行文件：

1. 构建并签名现代入口 `DeskLink.exe`；
2. 把已签名应用嵌入单文件安装器；
3. 构建并签名最终 `DeskLinkSetup-<version>-x64.exe`；
4. 对两个签名执行 Authenticode 发布策略和时间戳验证。

没有配置签名身份时，`python scripts/build-windows-installer.py` 仍会生成明确标记为 `unsigned` 的本地测试包。发布包不应跳过签名。

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

## 单文件验证与故障定位

只签一个已构建文件：

```powershell
python scripts/sign-windows-artifact.py dist\windows\DeskLinkSetup-0.1.1-x64.exe
```

只验证已有签名和 RFC 3161 时间戳：

```powershell
python scripts/sign-windows-artifact.py --verify-only dist\windows\DeskLinkSetup-0.1.1-x64.exe
```

脚本默认查找最新 Windows SDK x64 `signtool.exe`；如果 SDK 在自定义位置，可设置 `DESKLINK_SIGNTOOL`。任何签名或验证警告/失败都会让构建失败，避免误发布未签名或未加时间戳的包。

自签名证书只适合开发机或已通过组策略分发根证书的内部环境，不适合公开下载。证书续期时替换证书存储中的证书并更新指纹即可；已正确加 RFC 3161 时间戳的旧版本仍可在签名证书过期后验证。
