# DeskLink 应用账号与远程连接边界

## 目标

DeskLink 现在要求用户先登录应用，之后才能进入连接设备、共享设备和控制页面。账号只解决“谁可以使用这份软件”，远程连接仍然解决“这台设备是否允许控制那台主机”。两套安全链路故意保持独立。

## 用户流程

1. 用户使用邮箱和至少 12 个字符的密码注册。
2. 系统发送邮箱验证链接；验证成功后才能登录。
3. 登录时登记当前安装实例的应用设备 ID，并返回短期 access token 和可轮换 refresh token。
4. access token 过期时客户端只尝试一次 refresh；refresh replay 会撤销整个 session family。
5. 忘记密码只返回统一的 202 响应，不泄露邮箱是否存在。重置成功后撤销该账号的全部应用会话。
6. 退出登录先释放本机远程输入并断开会话，再清除本机保存的远程连接材料和账号 token。

## 多设备语义

同一个账号可以同时登录 Windows、macOS 和多个 iPhone。账号服务只登记这些应用设备，设备之间不共享以下内容：

- 已保存主机记录；
- PairingInvite、relay authentication 和自动重连材料；
- 远程主机的可信控制端权限；
- 任何远程连接私钥。

所以新设备登录后仍必须重新扫码/输入连接码，并由主机重新审批。账号退出也不会自动撤销目标主机上的可信控制端；这必须在主机的“已批准设备”中单独操作。

## 数据流

```text
客户端登录
    │ 仅发送邮箱、密码、应用设备 ID
    ▼
账号服务 ── 邮件 provider
    │ 返回 opaque access/refresh token
    ▼
客户端安全存储

客户端配对 ── PairingInvite / Noise / relay session ── 远程主机
       （不经过账号服务，不携带账号 token）
```

## 客户端存储

- macOS/iOS：账号会话和安装实例 ID 使用独立 Keychain service。
- Windows：账号会话使用当前 Windows 用户 DPAPI 保护的文件，前端 WebView 永远拿不到 token。
- 退出登录会删除账号会话和当前设备的远程连接材料；本机设备身份密钥保留，以便未来登录后作为同一物理安装实例重新配对。

## 发布前必须确认

- 账号服务已经部署到 HTTPS 域名，且 `DESKLINK_ACCOUNT_ORIGIN` 与邮件链接一致。
- 生产 SQLite 使用持久卷并有可恢复备份。
- 邮件 provider 的 API token 只配置在服务端，验证邮件和重置邮件能实际到达。
- Windows、macOS、iOS 客户端的账号 URL 已指向同一个服务。
- 真机上分别验证新设备登录不会出现历史主机，退出后本机保存设备为空。
