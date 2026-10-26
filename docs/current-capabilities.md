# 当前能力与阶段证据

更新日期：2026-10-26。用户已确认 **Windows→Windows、macOS→Windows 的基础连接、画面与控制良好**。这条确认保留为产品事实；高级功能、特殊权限、故障恢复与性能仍分别核验。

**当前工程分支不可作为可用整包交付。** 后续已复用 DL2-013 wire/codec 合同，但生产 context/epoch 仍为零；编译与 codec 测试通过，host runtime 业务测试实际失败。此过渡状态不改变用户确认的 main 基础路径，也不能被先前 probe 接入的绿色报告覆盖。

## 源码身份与版本

本矩阵的功能实现基线是 `main@a905b8807c83677f6f962c18f8fd1593f0d4ca32`，初始工程入口切片是 `44a2a997834d01157ae27a08bc810804422a0f72`。后续工程 probe 接入源码快照为 `673ec15a50afdffa06f9d65462a568ad2bb8f440`，再复用 input context 合同后为 `5501aad9f7d160f751048036ac2a14cf0a315680`；尚不包含 host/发送/呈现接线及其他分支完整修复，也不是 main 已整合。下面的功能矩阵仍按 main 基线逐项说明；当前工程 checkout 的协议版本和失败状态单列。

复用来源：协议 `9979c6424cf99f704feb212c7700833fce466dae` → 本分支 `e0d8df8a10fecab5818e04b999fbd424b12b933f`；shared-probe `3277c2a41af0ac923e1daa1c5cab5d952b355b9b` → `9c4119581508fc08c895aa8e3b24063ad03e657e`；probe lane `0836a8b311a858bf12f0d0b4427961c35f5986a7` → `673ec15a50afdffa06f9d65462a568ad2bb8f440`。shared-probe 的合并仅保留探测接线，排除依赖未引入 DL2-010 的两处 snapshot 格式调整；未为此引入整个媒体分支。接入测试另有带源码身份的报告，不用来源提交的测试结果替代。

input context 合同 `bc6c0a62d3817fbe44759cddf5069c3dee117f4c` → `5501aad9f7d160f751048036ac2a14cf0a315680`。字段、非零校验和统一输入前缀已落地；`ControllerRuntime::send_input` 的显式 InputContext 参数和两平台生产者还未接通，不得在发送时补当前值。Windows HostVideoPipeline、FFI host 配置和两处 controller 输入生产者仍显式为零；非零校验会拒绝，不可用默认 1 绕过。

版本从 manifest/配置读取，命令只读，不构建、不访问设备或网络：

```sh
python scripts/verify-windows-release.py --versions-only
```

| 项目 | 核对值 | 权威来源 |
| --- | --- | --- |
| Windows 应用 | `0.1.93` | `apps/windows/Cargo.toml`、`apps/windows-ui/package.json`、`apps/windows-ui/src-tauri/Cargo.toml`、`tauri.conf.json`、`tools/windows-installer/Cargo.toml`，脚本要求五者一致 |
| 业务协议 | `13` | 当前工程 checkout 的 `crates/desklink-protocol/src/lib.rs::PROTOCOL_VERSION`；未发布开发线，已复用 InputState 契约，不是应用版本或 relay JOIN 信封版本 |
| main 基线业务协议 | `12` | `main@a905b88`；不能把工程接入分支当作 main 已集成 |
| Bun | `1.3.14` | 三个 Bun 项目的 `packageManager`；本地入口检查实际可执行版本 |
| Rust toolchain | `1.97.1` | `rust-toolchain.toml`；workspace `rust-version=1.88` 是最低版本要求 |

Windows 0.1.93 是公开测试构建，测试更新清单有独立签名；尚无 Windows Authenticode 正式发布验收。历史 0.1.91/0.1.92 的协议与安装记录保留原日期和范围，不能替代本阶段验收。

产品分支在 `9979c6424cf99f704feb212c7700833fce466dae` 建立了 **协议 13** 的 `InputState { reason }` 契约，原因是 Blocked / UnsupportedKey / InvalidInput，`None` 表示恢复；仅末尾追加 postcard variant，relay JOIN 信封不变。本工程已为 probe 接入复用该独立契约，main 基线仍为 12；输入反馈/UI/旧端静默握手恢复不能仅凭契约提交算完成。DL2-013 后续合同沿同一未发布13开发线，新增非零 input_epoch/stream、统一 `[0, version:u16 LE]` 输入前缀及 exact 解码；wire/codec 通过不等于 host 原子校验、事件入队 context 与实际首帧呈现门禁已接通，严格绑定仍未完成。

## 方向与功能矩阵

“用户确认”只覆盖用户已确认的基础路径；“源码接通”表示本基线有生产调用链，本次本地测试不会自动把它提升为真实硬件或全部场景验收。“未接通”表示平台 UI/桥接缺失，不能因为 Rust 底层有方法而写作支持。

| 能力 | Windows→Windows | macOS→Windows | Windows/macOS→被控 Mac | iOS→Windows |
| --- | --- | --- | --- | --- |
| 基础连接、画面、控制 | **用户确认良好**；本地 Rust/TS 自动检查通过 | **用户确认良好**；本次未运行 Swift/Xcode | HostBridge、ScreenCaptureKit、VideoToolbox、CGEvent 已有实现；独立真机验收 | AppleCore、Metal、触控模型已有；本次无物理 iPhone 结果 |
| 键盘、按键释放 | 源码接通；滚轮/状态反馈等 DL2 修复仍按编号验收 | AppKit keyDown/keyUp 已接通；修饰键/字符释放修复见 Apple 分支证据 | MacInputInjector 已有；真实权限与异常释放另验 | 触控/键盘模型已有；不推导所有硬键盘场景支持 |
| 中文与 IME | 文本提交链已有；特殊窗口/组合输入仍需实测 | 基线 SessionInputView 未接 NSTextInputClient；DL2-007 未由基础控制确认关闭 | 编码码流不等于 IME 注入能力；独立核验 | 共享文本模型不等于实际输入法接线验收 |
| 系统音频 | Windows loopback→Opus→控制端播放链已有；音画延迟未实测 | 基线 Swift 会话未消费/播放 audio 事件 | Mac host 无已验收系统音频链 | 基线 Apple 控制桥无已接通音频播放 |
| 纯文本剪贴板 | 用户主动触发链已有；不自动监听 | 基线无会话 UI/桥接闭环 | 无已验收闭环 | 无已验收闭环 |
| 文件、队列与恢复 | 发送/确认/续传链已有；全队列、拖放命中、慢盘等按 DL2 分项 | 基线无选择/接收/队列/恢复 UI 闭环 | 无已验收完整文件授权/接收闭环 | 无已验收文件闭环 |
| 多屏、缩放与光标 | 会话内切屏、适应/1:1/平移/全屏已有；拓扑、DPI 与恢复另验 | 有原生窗口/全屏习惯；无会话切屏/1:1/平移工具闭环；cursor 事件已存储但未呈现独立 overlay | 基线主屏捕获；多屏声明、尺寸和真实编码需独立核验 | 触控几何模型已有；不推导 Windows 桌面工具等价 |
| 批准、权限、主动收回 | 原生批准/撤销/释放链已有；UAC 安全桌面与 Ctrl+Alt+Delete 不支持；锁屏等另验 | Windows host 原生批准；Mac 控制权限/焦点行为另验 | 屏幕录制/辅助功能权限门禁已有；授权撤回、签名/公证另验 | iOS 仅 controller，不是 host |
| 重连、取消、恢复 | 生产状态机已有；故障、静态画面、背压、长稳逐项验收 | 基线恢复错误处理和取消有 DL2-003/004/029 缺口；分支修复不等于已集成 | Host 恢复与能力协商独立验收 | AppleCore 修改还须 iOS 回归 |

主要源码定位：`apps/windows-ui/src/controller.ts` 与 `src-tauri/src/controller.rs`、`apps/windows/src/runtime.rs`；`apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift::consume`；`apps/macos/Sources/DeskLinkApp/Views/SessionView.swift`、`Input/SessionInputView.swift`、`Bridge/HostBridge.swift`、`Capture/MacH264Encoder.swift`。Mac 编码器在基线已有真实 VideoToolbox 实现，不能再写成未来空白计划；它与协商 Main/1080p 的声明漂移是独立 DL2-043 问题。

## 本次自动验证与历史记录

工程切片的 Windows 本地结果：三个 Bun 项目测试 246 / 20 / 14 项与类型检查、前端生产构建、Python 127 项、Rust fmt/Clippy/workspace lib、协议与会话契约、localhost transport/relay、FFI runtime、媒体恢复及 Windows runtime 契约通过。原始命令/日志见本工作区 `dist/verify/verified-local.json`；记录的 source_commit 为 `a905b88`、dirty=true，表示基线加当时工程切片，而非 main 已整合所有修复。后续文档元数据检查另有独立结果。

`native perf` 在当前 Windows 环境实际记录 Apple、未授权交互设备、未集成离线基线和完整性能条件为 skipped，报告 incomplete / passed=false。未运行真实远控、输入注入、线上发布/探针、Swift/Xcode、双机、GPU/CPU 对照或长稳。入口说明见[分层本地验证](local-verification.md)。2026-07-26 的 Apple、旧 Windows 发布冻结和其他历史报告保留原范围；第二阶段以 [TODO](../TODO.md) 前九节及本矩阵为准。

最新合同过渡报告 `dist/verify/input-context-transition.json` 记录源码 `5501aad`、dirty=true：workspace 全target/feature编译、protocol49项、crypto/transport79项通过；host runtime 9通过/1失败，生产 `HostVideoPipeline::prepare` 的零epoch被拒绝为 `Protocol(InvalidVideoConfig)`，报告整体 failed / passed=false。它是待owner接线的业务失败，没有 skip、填默认值或改断言标绿。此前 `probe-integration.json` / `probe-clippy.json` 的源码是 `4c3648a`，仅证明此前 probe 接入，不能证明新过渡源码可运行。

## 独立分支的已提交证据快照

下表只记录指定 commit 及其已提交 TODO/测试证据，工程分支未重跑这些分支实现，也未把它们合入 main。后续提交不自动包含在此快照中。整合时保留逐问题提交，只合并对应 TODO 证据。

| 分支 / 固定快照 | 已提交范围 | 尚不能推导的结论 |
| --- | --- | --- |
| `codex/dl2-windows-product` · `9979c6424cf99f704feb212c7700833fce466dae` | DL2-001 滚轮、034 草稿、031 完整队列、032 拖放目标、009 协议 13 契约；035 是 `776a807`→`ceebb28`→`61438de` 三笔完整链 | 035 中间提交不能单独交付；032 真 DPI/双机拖放未验；009 非整体完成 |
| `codex/dl2-apple-ffi` · `8950ad0249c47d500bb394697636c80e3043a0fd` | 003 可恢复错误、004 取消/销毁、005 按键释放、029 等待取消、043 Main/尺寸安全切片 | Rust/静态合同不等于 Swift/Xcode、IME、权限和真实 Mac 完成；043 动态完整能力仍有边界 |
| `codex/dl2-windows-media` · `4862e95565eeb8725f9fd5687c11addeaaa3bcac` | 010 静态关键帧、011 探测锁、012 输入隔离、013 显示器身份、014 离线基线、021 host 慢盘隔离、022 DXGI 恢复、030 单次流式哈希 | 021 控制端接线仍由产品组继续；离线合成指标不能替代双机总延迟或性能提升 |
| `codex/dl2-network` · `e173cf26f975f1cd28efe727d2afa4ce3aa4f02c` | 019 中继媒体队列与连接读循环隔离 | 尚非 024 全通道直连，弱网/真实 WAN 未由本工程分支证明 |
| `codex/dl2-services` · `6b4878dab987097936cd51446f6cf2fea0e7f858` | 036 原子幂等注册、037 可写数据库 readiness、039 索引/分片保留期清理，有本机合成证据 | 未向生产部署；邮件/真实服务、长期磁盘硬上限与生产性能不由合成数据替代 |

发布前的 Authenticode、安装/SmartScreen、签名/公证和真实设备证据保持独立；它们的缺项不能被重新解释为用户已确认的基础远控没有实现。
