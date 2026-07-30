# DeskLink 当前架构契约

更新日期：2026-07-31
状态：当前实现与发布边界

本文是 DeskLink 当前版本的产品、架构和发布验收契约。实现细节以当前源码、测试和[Windows 发布运行手册](windows-release-runbook.md)为准；[早期详细设计](Windows_macOS_iOS_个人远程桌面软件详细设计与开发文档.md)保留为设计输入和历史决策记录，不再单独定义当前版本的验收范围。

## 1. 产品边界

- Windows 10/11 x64 是当前唯一正式发布目标；当前候选版本为 `0.1.91`，应用协议版本为 `12`。
- Windows 支持不登录账号的本地模式。可选账号只用于邮箱验证、账号会话和安装实例可见性，不替代设备配对、主机批准或端到端加密认证。
- macOS Apple Silicon 同时保留 controller/host 开发与验收能力；iOS 16+ 只作为 controller，不对外宣称能托管或控制 iOS 系统。
- relay 只负责会话匹配和转发业务密文；它不能读取远程桌面、键鼠、剪贴板或文件内容。
- DirectLan 是有认证、短生命周期、会话绑定的视频数据面优化，只传视频，失败时必须无感回退到 relay，不能结束控制会话。

当前版本明确不承诺：公网上开放注册、多租户权限管理、iOS host、UAC/安全桌面控制、4K/高帧率专业采集、无人值守静默升级，以及未经独立验收的生产级高可用 relay/account 服务。

## 2. 平台职责

| 平台 | 角色 | 当前职责 | 发布状态 |
| --- | --- | --- | --- |
| Windows | controller/host | 正式远程连接、屏幕采集与编码、输入注入、剪贴板/文件、批准和本地诊断 | `0.1.91` 候选版 |
| macOS Apple Silicon | controller/host | ScreenCaptureKit、VideoToolbox、输入和批准桥接 | 独立开发与真实权限验收 |
| iOS 16+ | controller | 连接、批准状态、视频解码和触控输入模型 | 独立开发与真实设备验收 |
| relay | 中继 | 配对后的会话匹配和密文转发 | 已有运行探针；HA 仍需独立建设 |
| account | 可选服务 | 邮箱验证、账号会话、安装实例登记 | 不等同于 relay 就绪，需独立部署验收 |

## 3. 连接与数据平面

```text
Windows/macOS/iOS UI
        |
platform adapter: capture / encode / input / permission / native storage
        |
Rust FFI: protocol / crypto / session / transport / cancellation
        |
relay-default QUIC/TLS -------- opaque encrypted control/data -------- peer
        |
authenticated DirectLan (video only, optional, short-lived)
```

Rust 共享层拥有协议、设备身份、配对邀请、Noise 会话、业务 AEAD、重连、流量边界、取消和恢复状态机。平台层拥有操作系统能力：屏幕/音频采集、视频编码解码、鼠标键盘/触控注入、权限批准、Keychain/DPAPI 和 UI 呈现。平台层不得复制协议或安全判断。

控制、批准、输入、剪贴板和文件传输走可靠的加密业务通道；视频走有明确上限的分片/重组通道，并可选择 DirectLan。任何直连探测、网络切换、数据通道关闭或超时都必须回落到 relay，并保留控制会话和用户可理解的状态。

## 4. 不可破坏的安全与生命周期约束

1. host 在批准前不得采集或发送屏幕/视频配置，不得注入键鼠；批准状态必须由 host 本地原生 UI 控制。
2. 关闭、断线、重连、页面销毁、采集丢失和权限撤销都必须触发 `ReleaseAll`；事件不能因 channel 关闭而静默丢失，后台 worker 必须可取消并等待收尾。
3. 设备身份密钥只进入 DPAPI/Keychain 保护的持久化层；UI、日志和诊断报告只输出脱敏摘要，不输出私钥、口令、邀请 token 或业务明文。
4. relay 和 diagnostics 都是独立边界：relay 只看路由元数据与密文，诊断采集必须显式开启并可审计；健康 relay 不代表 account 服务已经部署。
5. 过期邀请、认证不匹配、协议不匹配、流量超限和不可恢复的状态错误必须停止盲目重试，并返回可诊断的终态。

## 5. 模块所有权

| 模块 | 唯一所有权 | 禁止事项 |
| --- | --- | --- |
| `desklink-protocol` | 帧、消息、版本和长度边界 | 平台代码自定义同义消息 |
| `desklink-crypto` | 身份、配对、Noise、AEAD | 日志暴露密钥或明文 |
| `desklink-transport` | QUIC/TLS、relay/direct 连接 | 绕过认证建立数据面 |
| `desklink-session` / `desklink-video` | 重连、恢复、视频分片 | 无界缓存和无界重试 |
| `desklink-ffi` | 跨平台 C ABI、worker、取消 | 在 UI 层复制 Rust 状态机 |
| `apps/windows` | Windows 原生能力和 host 安全边界 | 用脚本或 WebView2 代替批准边界 |
| `apps/windows-ui` | Windows 发布 UI、状态和用户操作 | 宣称未验收的平台能力 |
| `apps/apple` / `apps/macos` / `apps/ios` | Apple 能力桥接与独立验收 | 把 iOS 当作 host 发布 |
| `relay` | 会话目录与密文转发 | 保存或解析业务明文 |
| `account` | 账号生命周期和实例登记 | 默认为远程授权或 relay 健康证明 |

## 6. 发布门禁

自动化门禁包括 Rust format/Clippy/workspace tests、Windows UI test/build、发布版本与清单哈希、安装器负载校验、Apple 构建/测试和 managed relay 双向探针。它们证明代码和受控环境满足约束，但不能替代真实设备验收。

`0.1.91` 只有在以下条件全部具备时才能称为正式 Windows 发布：干净且可复现的源代码 commit、已验证并签名的安装器、两台真实 Windows 设备上的配对/批准/视频/输入/剪贴板/文件/断线恢复与睡眠唤醒记录、长时间运行结果，以及对应的回滚证据。缺少签名、真实设备记录或 managed diagnostics 报告时，状态只能写作 candidate，不创建正式 release tag。

relay 探针通过只说明当前 relay 能够工作；account DNS、HTTPS、容器、数据库、邮箱和 smoke test 必须按独立服务清单另行验证。
