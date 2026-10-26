# DeskLink 当前架构契约

更新日期：2026-10-26
状态：当前实现与发布边界

本文是 DeskLink 当前版本的产品、架构和发布验收契约。实现细节以当前源码、测试和[Windows 发布运行手册](windows-release-runbook.md)为准；[早期详细设计](Windows_macOS_iOS_个人远程桌面软件详细设计与开发文档.md)保留为设计输入和历史决策记录，不再单独定义当前版本的验收范围。

## 1. 产品边界

- Windows 10/11 x64 是当前唯一正式发布目标；main `a905b88` 产品基线为 `0.1.93` 公开测试构建、业务协议 `12`。第一波软件候选的已测试源码为 `a4c5b404d43794d646394923a481f23c9298c3ed`，业务协议为 `13`，尚未合入 main；当前 checkout 的应用/协议/工具链用 `python scripts/verify-windows-release.py --versions-only` 读取。[能力与证据矩阵](current-capabilities.md)分别保留 main 历史基线、候选来源和实际验证范围。
- 工程 `5501aad`、`56f53b4`、`d6f153e` 阶段尚未接通 host/发送/呈现，零 epoch 被 codec 拒绝并导致业务测试失败；这些结果保留原来源，不能外推为当前候选仍有零 producer 或七处旧生产调用未迁移。当前候选已保留 J 的非零 Windows producer/host 和完整 Apple/FFI 后继接线；仍禁止默认1、发送时补当前 epoch 或跳过失败绕过门禁。
- Windows 支持不登录账号的本地模式。可选账号只用于邮箱验证、账号会话和安装实例可见性，不替代设备配对、主机批准或端到端加密认证。
- macOS Apple Silicon 同时保留 controller/host 开发与验收能力；iOS 16+ 只作为 controller，不对外宣称能托管或控制 iOS 系统。
- 用户已确认 Windows→Windows、macOS→Windows 基础远控良好。Mac 的 IME、音频、剪贴板、文件、多屏、特殊恢复与真实权限分别核验；ScreenCaptureKit/VideoToolbox host 已有实现，不能写成未来空白计划，也不能用已有 Rust API 代替 Apple UI/桥接闭环。
- relay 只负责会话匹配和转发业务密文；它不能读取远程桌面、键鼠、剪贴板或文件内容。
- DirectLan 是有认证、短生命周期、会话绑定的视频数据面优化，只传视频，失败时必须无感回退到 relay，不能结束控制会话。

当前版本明确不承诺：公网上开放注册、多租户权限管理、iOS host、UAC/安全桌面控制、4K/高帧率专业采集、无人值守静默升级，以及未经独立验收的生产级高可用 relay/account 服务。

第一波候选完整来源为 J `5782341`、Apple `f954a68` 与 input admission 后继 `cb7514c`、网络 `4b155615`、工程 `6b31360`、GUI `50691585`、诊断 producer `28c11ee` 和 services `d2d4f1c`。`a4c5b40` 的 v3 共25个软件门禁全部通过：Python257、Bun前端474/账号79/诊断129、FFI91（含真实 MSVC C ABI）、Windows292通过/17忽略、relay64通过/3忽略；任务计数有重叠，不累加成独立覆盖率。19个 fresh 真实 HTTP 案例按预期通过，正例直接使用原始 RustSignedRequest 的 body/hash/签名，不以改写或重签替代，覆盖 schema、文件 SQLite、重开与去重；范围为7事件、24个 event/field 对、18个不同 u64 键，另有旧 unsafe numeric 拒绝与篡改后401负例。详细原始报告与执行范围见[本地验证说明](local-verification.md)。

平台 native/perf 报告仍为 incomplete：2项通过（双头检查、CPU离线基线）、7项 skipped，忽略和跳过均不计通过。以下验收继续未完成：

- [ ] Swift/Xcode、六项 Swift Recording 用例与真实 Apple presented 行为。
- [ ] 实际 WebView、真实输入注入、权限、双机与显示拓扑验收，包括 Apple 同 stream 完整源拓扑/epoch 权威映射。
- [ ] 完整 Mac 功能、硬件/GPU、同条件性能比较和长稳、安装签名及正式发布门禁。

GPU235 后续独立候选、文件030/业务协议14与LAN024后续生产接线不合入本轮；这些后续功能不阻塞已完成的协议13软件候选验证。

第二波独立GPU候选从第一波文档冻结 `c3fa4df` 正常merge `23526f2` 的七笔历史；已测试源 `97ff7c08199cdad4511b0044ddf783d79e2295b1` 的25软件检查、24选定合成GPU用例与19签名/HTTP/SQLite案例全部通过，native/perf仍2通过7跳过、incomplete。第一波测试源a4与冻结c3不变，context/输出身份和公共协议不变；真实捕获/设备恢复、同画质性能与其余平台门禁保持未验。详见[第二波报告](wave2-gpu-integration.md)。

## 2. 平台职责

| 平台 | 角色 | 当前职责 | 发布状态 |
| --- | --- | --- | --- |
| Windows | controller/host | 远程连接、屏幕采集与编码、输入注入、剪贴板/文件、批准和本地诊断 | `0.1.93` 公开测试版；正式签名/发布独立验收 |
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
6. 控制端、主机端的 relay 建连、join 和目录查询必须有明确的超时；取消、网络黑洞和中继无响应不能让连接 worker 无界等待。

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

第二阶段的现状差异、公共错误/资源合同和后续迁移见[所有权与边界合同](second-stage-boundary-contracts.md)。DL2-040 已实际拆分共享 transport 错误分类并接通 Host/Controller 两个生产入口；其余具体 owner 迁移仍逐切片推进，无需等全部功能完成后才继续。巨型 controller/runtime/FFI 模块整体拆分及性能收益尚未完成验收。

## 6. 发布门禁

本地验证统一使用 `scripts/verify.py` 的 fast/integration/native/perf，覆盖 Rust、三个 Bun 项目和 Python；工具、源码、命令和 skipped 语义见[本地验证说明](local-verification.md)。Windows 发布另有版本/清单哈希与安装器负载门禁；Apple 构建/测试及 managed relay 双向探针是独立显式入口，不因本地通过而视为已执行。它们均不能替代真实设备验收。

当前候选只有在以下条件全部具备时才能称为正式 Windows 发布：干净且可复现的源代码 commit、已验证并签名的安装器、两台真实 Windows 设备上的配对/批准/视频/输入/剪贴板/文件/断线恢复与睡眠唤醒记录、长时间运行结果，以及对应的回滚证据。缺少签名、真实设备记录或 managed diagnostics 报告时，状态只能写作公开测试/candidate，不创建正式 release tag；这些门禁缺项不否定用户已确认的基础远控路径。

relay 探针通过只说明当前 relay 能够工作；account DNS、HTTPS、容器、数据库、邮箱和 smoke test 必须按独立服务清单另行验证。
