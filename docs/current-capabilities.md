# 当前能力与阶段证据

更新日期：2026-10-26。用户已确认 **Windows→Windows、macOS→Windows 的基础连接、画面与控制良好**。这条确认保留为产品事实；高级功能、特殊权限、故障恢复与性能仍分别核验。

**当前工程分支不可作为可用整包交付。** 已复用 DL2-013 wire/codec 与显式 Rust 输入 API 桥接，后者加密loopback通过；七处旧生产调用仍未迁移，host仍发布零epoch，host runtime业务测试实际失败。此过渡状态不改变用户确认的 main 基础路径，也不能被局部绿色报告覆盖。

## 源码身份与版本

本矩阵的功能实现基线是 `main@a905b8807c83677f6f962c18f8fd1593f0d4ca32`，初始工程入口切片是 `44a2a997834d01157ae27a08bc810804422a0f72`。后续 probe 接入源码快照为 `673ec15a50afdffa06f9d65462a568ad2bb8f440`，input context 合同后为 `5501aad9f7d160f751048036ac2a14cf0a315680`，显式API桥接后为 `56f53b4a17347714a91a2cb269bc5addc5777916`；尚不包含 host/生产者/呈现完整接线，也不是 main 已整合。下面的功能矩阵仍按 main 基线逐项说明；当前工程 checkout 的协议版本和失败状态单列。

复用来源：协议 `9979c6424cf99f704feb212c7700833fce466dae` → 本分支 `e0d8df8a10fecab5818e04b999fbd424b12b933f`；shared-probe `3277c2a41af0ac923e1daa1c5cab5d952b355b9b` → `9c4119581508fc08c895aa8e3b24063ad03e657e`；probe lane `0836a8b311a858bf12f0d0b4427961c35f5986a7` → `673ec15a50afdffa06f9d65462a568ad2bb8f440`。shared-probe 的合并仅保留探测接线，排除依赖未引入 DL2-010 的两处 snapshot 格式调整；未为此引入整个媒体分支。接入测试另有带源码身份的报告，不用来源提交的测试结果替代。

input context 合同 `bc6c0a62d3817fbe44759cddf5069c3dee117f4c` → `5501aad9f7d160f751048036ac2a14cf0a315680`，字段、非零校验和统一输入前缀已落地。最小显式API桥接 `b473498382bd163f6085c019521105741264707c` → `56f53b4a17347714a91a2cb269bc5addc5777916`，仅复用 `send_input_with_context` 与其加密loopback测试；旧send_input继续拒绝，七处生产调用尚未迁移，Windows HostVideoPipeline、FFI host 配置和旧输入路径仍显式零，不能默认1或发送时取当前epoch。

该桥接的 controller.rs/test 两文件 hunk 与完整Apple `9ad7154b365de875d725188cfa091f5c21da0b5a` 等价，稳定patch-id为 `2f004b5c19baaa5f3df4360b605818df1f40f9b4`；后续按等价patch去重，不能重复覆盖。完整9ad的C头、lib.rs、worker、ledger与其003/004/025前置没有被本工程引入，C/Swift接线仍以Apple源分支为准。

后续独立Sender来源 `3ea72dc7f5a66d4cd96c197c0aadb7603cee2436` → 本工程 `d6f153ed59c6d91b7cfa7337dd1b1a9fbeae9cf6`，只依赖bc6+b473。Runtime与可clone的ControllerInputSender共用单一发送实现/序号；输入专用序号锁持到完整写入，加密锁不跨网络等待。该切片继续扩展上述桥接，后续整合须保留Sender与测试，不用完整Apple文件覆盖；未引入C/Swift/worker祖先，七处生产旧调用仍未迁移。

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

最新合同过渡报告 `dist/verify/input-context-transition.json` 记录源码 `5501aad`、dirty=true：workspace 全target/feature编译、protocol **47项**、crypto/transport79项通过；host runtime 9通过/1失败，生产 `HostVideoPipeline::prepare` 的零epoch被拒绝为 `Protocol(InvalidVideoConfig)`，报告整体 failed / passed=false。来源分支报告49项，本工程实际执行47项，计数不能照搬。它是待owner接线的业务失败，没有 skip、填默认值或改断言标绿。此前 `probe-integration.json` / `probe-clippy.json` 的源码是 `4c3648a`，仅证明此前 probe 接入，不能证明新过渡源码可运行。

API桥接后的 `dist/verify/captured-context-bridge.json` 记录干净源码 `56f53b4`、dirty=false：编译、protocol47项及 controller加密loopback5项通过（0忽略）；host runtime仍9通过/1失败，整体failed / passed=false。这是新API原语通过，不是七处生产调用、完整C/Swift/FFI、host epoch或实际呈现已接通；DL2-013严格绑定继续未完成。

有序Sender后的 `dist/verify/ordered-input-sender.json` 记录干净源码 `d6f153e`、dirty=false：fmt、编译、protocol47、独立controller-loopback6通过；完整ffi-runtime内lib12、ABI7、platform1、controller6、directory4通过，但Host8通过/2失败（旧配置epoch0），所以完整FFI失败；Windows host仍9通过/1失败。此失败仅属于尚未整合完整Apple ABI/Host依赖的本工程旧源码，不能外推为Apple当前源分支失败。加密回归实际覆盖32并发clone和Runtime共36帧连续序号、原context保留、零context不发包/不消耗序号，仍不证明生产迁移、新Noise随机epoch或IPC大整数已完成。整体报告failed / passed=false，不以选中项通过替代完整结果。

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

## 新增审查的逐条归因

根目录未提交044～061草稿不是工程组或本组子agent所写，作者仍未确定，本组未stage/覆盖。下表只收录已核对的反证和已有修复，不把其余新项概括成“全部确认”；详细来源、范围和待验用例见[审查证据记录](second-stage-review-evidence.md)。

| 条目 | 当前归因 | 验证/完成边界 |
| --- | --- | --- |
| 044 | 锁定Quinn拥塞淘汰旧包返回Ok，公开错误无QueueFull，wait内部Blocked等待；原一次拥塞结束会话的P1论证不成立 | 剩余cursor真实能力/尺寸/连接错误传播及可选通道降级，由媒体实际loopback/禁用对端/超限/终态回归；observer尝试/错误数不是实际丢包 |
| 045 | Some(Err)原实现已continue；原瞬态Err永久禁用叙述不成立 | 剩余None终态可观测性和失败握手恢复验证，不重复算已有正确逻辑的修复 |
| 046 | 取消树已由独立3277c2a实现，本工程已有9c41195等价提交 | 完成仲裁/覆盖仍待受控时序验证，不能由JoinSet存在直接关闭整项 |
| 048 | key经Noise message3 payload认证解密；未直接纳入Ed签名不足以证明已确认P1/网络替换或UKS路径 | 熵源/派生防御纵深与篡改/拼接测试待评估；不据此直接改crypto或背书新缺陷严重性 |

上述分类没有新增真实远控/性能证明，也没有修改其他编号完成状态；其他新项继续按owner复核当前分支后逐项记录。

## 初始化保留缺口：源分支已修、joint待复验

协调方joint只读审查与本工程固定媒体 `89bd1e5161653dd668d290a417bd5f6cc146290a` 源码核对确认过DL2-021初始化缺口：打开已有resume partial后，ready发送失败直接return，未走preserve，IncomingFile的remove_on_drop=true导致partial被删。媒体已独立提交 **`65c70fd658d0526a8a27b75fdbf120ae065222bf`** 修复，在同一磁盘线程先preserve后退出；本工程只读Git确认parent为 `6dbe53d64c60bbdd50af4b592ebef7471b800fcc`，仅TODO+transfer变更。源TODO及协调方报告transfer28项、lib/tests严格Clippy通过，包含真实open barrier/drop future、9次prefix保留及完整续传等回归；本工程未复跑。当前为**源分支修复已验证、joint待复验**，不能继续将65c视为未修源码，也不能提前标joint通过；详细链路见[证据记录](second-stage-review-evidence.md)。

此前协调方报告的joint未提交/索引UU/测试未跟踪，是当时的审查快照；本工程未检查其WIP，不能将该快照当作当前状态。其日志计数已更正为shared174通过/1忽略（原274笔误撤回），不是本工程实际测试，也不能替代最终jointSHA/报告。除此项之外“暂未见新确定缺陷”的只读review表述不等于测试通过。FFI Mac Host/Swift、实际WebView和双机仍独立；本工程原始失败报告继续按各自源码身份保留。

## 联合收口来源更新（2026-10-26）

| 来源 | 已核对或收到的证据 | 当前边界 |
| --- | --- | --- |
| 产品 `27fec756ba97ee6f67228dc9530c7b0578a49ff3` | 本工程只读Git对象确认其parent为 `16594c4f8780cd565afd57695f88e29277ec5939`；代码仅将既有owned future提取为 `InputSendFuture<Writer>` 类型别名，发送语义不变。提交TODO报告UI库135通过/1项签名资产忽略、本包 `--lib --tests --no-deps` Clippy通过 | 本工程未复跑、未复用整包；本包Clippy不代表跨包严格检查。最终joint第一parent采用27fec，保留既有历史 |
| Apple最新源分支 | 协调方报告完整FFI71项、Python186项、双头检查实际通过；013 worker owned future仍在进行 | 本次报告未附对应完整测试SHA，不把所见branch head擅自绑定到该结果；等待最终稳定SHA与完整依赖一起合并，不能单独flat-pick9ad。旧joint两个MacHost失败来自尚缺完整Apple ABI/Host链，不能写成Apple当前分支失败；源分支通过不等于最终joint或Swift/真机呈现通过 |
| 媒体021修复及最终joint | 另一parent已固定为 `65c70fd658d0526a8a27b75fdbf120ae065222bf`，源TODO及协调方报告transfer28与严格Clippy通过；协调方报告已开始27fec+65c真实remerge，10份人工resolution/新增联合测试已备份并有SHA256清单 | 最终jointSHA/完整报告仍待交付。本工程未复跑来源测试、未读取备份清单或joint WIP；备份不等于文件已纳入最终提交，合并后须重跑跨包严格检查及所有适用测试 |

原产品16594与媒体89bd的合并安排、旧joint审查和工程d6f153e失败报告均保留其历史身份。021来源层已有修复及28项通过报告，联合层仍待复验；上述来源进展不关闭041联合验收、013完整生产接线或真实设备门禁。
