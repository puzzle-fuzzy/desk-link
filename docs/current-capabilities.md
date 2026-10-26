# 当前能力与阶段证据

更新日期：2026-10-26。用户已确认 **Windows→Windows、macOS→Windows 的基础连接、画面与控制良好**。这条确认保留为产品事实；高级功能、特殊权限、故障恢复与性能仍分别核验。

**第一波协议13候选已冻结并通过本地软件门禁。** 已完整合入Windows joint、Apple及输入退役补丁、网络、工程、GUI和producer/services历史，保留非零Windows producer；测试源为 `a4c5b404d43794d646394923a481f23c9298c3ed`。25项软件检查及新生成原生签名批次的19项HTTP/SQLite闭环通过，详细范围见[第一波报告](wave1-integration.md)。Swift/Xcode、真实WebView、权限/输入注入、双机与完整性能仍未验收，main尚未合入。下列旧零producer失败均按其历史源码保留。

**当前第二波GPU候选亦已实际验证。** 独立分支从第一波冻结c3正常merge GPU235七笔历史；干净测试源 `97ff7c08199cdad4511b0044ddf783d79e2295b1` 的25软件检查、24选定合成GPU用例及19签名/HTTP/SQLite案例全部通过，native/perf仍2通过7跳过、incomplete。第一波a4/c3不变；真实捕获、设备恢复、同画质性能和原平台缺项保持未验。见[第二波报告](wave2-gpu-integration.md)。

## 源码身份与版本

### 历史工程复用链（44a2a99至d6f153e）

下方历史功能矩阵的基线是 `main@a905b8807c83677f6f962c18f8fd1593f0d4ca32`，初始工程入口切片为 `44a2a997834d01157ae27a08bc810804422a0f72`。早期probe接入为 `673ec15a50afdffa06f9d65462a568ad2bb8f440`，input合同为 `5501aad9f7d160f751048036ac2a14cf0a315680`，最小桥接为 `56f53b4a17347714a91a2cb269bc5addc5777916`；这些早期快照尚无完整生产接线。当前第一波已进一步合并完整历史，不能再用早期失败描述它；main仍未合入。

复用来源：协议 `9979c6424cf99f704feb212c7700833fce466dae` → 本分支 `e0d8df8a10fecab5818e04b999fbd424b12b933f`；shared-probe `3277c2a41af0ac923e1daa1c5cab5d952b355b9b` → `9c4119581508fc08c895aa8e3b24063ad03e657e`；probe lane `0836a8b311a858bf12f0d0b4427961c35f5986a7` → `673ec15a50afdffa06f9d65462a568ad2bb8f440`。shared-probe 的合并仅保留探测接线，排除依赖未引入 DL2-010 的两处 snapshot 格式调整；未为此引入整个媒体分支。接入测试另有带源码身份的报告，不用来源提交的测试结果替代。

input合同 `bc6c0a62d3817fbe44759cddf5069c3dee117f4c` → `5501aad9f7d160f751048036ac2a14cf0a315680`。最小桥接 `b473498382bd163f6085c019521105741264707c` → `56f53b4a17347714a91a2cb269bc5addc5777916` 当时仅复用显式API/loopback；七处生产调用及零producer尚未迁移。该历史失败不允许用默认1或发送时补当前epoch掩盖，现已由第一波完整来源整合取代。

桥接两文件hunk与完整Apple9ad等价，稳定patch-id为 `2f004b5c19baaa5f3df4360b605818df1f40f9b4`。早期没有引入完整C/worker/ledger；第一波现已合入f954+cb751完整历史并去重，保留Sender扩展，没有单挑9ad或整文件覆盖。

独立Sender来源3ea72dc → `d6f153ed59c6d91b7cfa7337dd1b1a9fbeae9cf6` 当时只依赖bc6+b473。其单一发送实现/序号、输入锁覆盖完整写入和crypto锁不跨I/O的语义已保留到第一波；当时未引入完整祖先的状况仅属d6历史。

### 当前候选版本

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

产品分支在 `9979c6424cf99f704feb212c7700833fce466dae` 建立了 **协议 13** 的 `InputState { reason }` 契约，原因是 Blocked / UnsupportedKey / InvalidInput，`None` 表示恢复；仅末尾追加 postcard variant，relay JOIN 信封不变。本工程已为 probe 接入复用该独立契约，main 基线仍为 12；输入反馈/UI/旧端静默握手恢复不能仅凭契约提交算完成。DL2-013 后续合同沿同一未发布13开发线，新增非零 input_epoch/stream、统一 `[0, version:u16 LE]` 输入前缀及 exact 解码；第一波已合入host/队列/呈现代码与本地联合回归；真实平台呈现、拓扑及端到端验收仍未完成，013主项保持未完成。

## main@a905b88历史功能基线

“用户确认”只覆盖用户已确认的基础路径；“源码接通”表示本基线有生产调用链，本次本地测试不会自动把它提升为真实硬件或全部场景验收。“未接通”表示平台 UI/桥接缺失，不能因为 Rust 底层有方法而写作支持。

| 能力 | Windows→Windows | macOS→Windows | Windows/macOS→被控 Mac | iOS→Windows |
| --- | --- | --- | --- | --- |
| 基础连接、画面、控制 | **用户确认良好**；本地 Rust/TS 自动检查通过 | **用户确认良好**；本次未运行 Swift/Xcode | HostBridge、ScreenCaptureKit、VideoToolbox、CGEvent 已有实现；独立真机验收 | AppleCore、Metal、触控模型已有；本次无物理 iPhone 结果 |
| 键盘、按键释放 | 源码接通；滚轮/状态反馈等 DL2 修复仍按编号验收 | AppKit keyDown/keyUp 已接通；修饰键/字符释放修复见 Apple 分支证据 | MacInputInjector 已有；真实权限与异常释放另验 | 触控/键盘模型已有；不推导所有硬键盘场景支持 |
| 中文与 IME | 文本提交链已有；特殊窗口/组合输入仍需实测 | 基线 SessionInputView 未接 NSTextInputClient；DL2-006 未由基础控制确认关闭 | 编码码流不等于 IME 注入能力；独立核验 | 共享文本模型不等于实际输入法接线验收 |
| 系统音频 | Windows loopback→Opus→控制端播放链已有；音画延迟未实测 | 基线 Swift 会话未消费/播放 audio 事件 | Mac host 无已验收系统音频链 | 基线 Apple 控制桥无已接通音频播放 |
| 纯文本剪贴板 | 用户主动触发链已有；不自动监听 | 基线无会话 UI/桥接闭环 | 无已验收闭环 | 无已验收闭环 |
| 文件、队列与恢复 | 发送/确认/续传链已有；全队列、拖放命中、慢盘等按 DL2 分项 | 基线无选择/接收/队列/恢复 UI 闭环 | 无已验收完整文件授权/接收闭环 | 无已验收文件闭环 |
| 多屏、缩放与光标 | 会话内切屏、适应/1:1/平移/全屏已有；拓扑、DPI 与恢复另验 | 有原生窗口/全屏习惯；无会话切屏/1:1/平移工具闭环；cursor 事件已存储但未呈现独立 overlay | 基线主屏捕获；多屏声明、尺寸和真实编码需独立核验 | 触控几何模型已有；不推导 Windows 桌面工具等价 |
| 批准、权限、主动收回 | 原生批准/撤销/释放链已有；UAC 安全桌面与 Ctrl+Alt+Delete 不支持；锁屏等另验 | Windows host 原生批准；Mac 控制权限/焦点行为另验 | 屏幕录制/辅助功能权限门禁已有；授权撤回、签名/公证另验 | iOS 仅 controller，不是 host |
| 重连、取消、恢复 | 生产状态机已有；故障、静态画面、背压、长稳逐项验收 | 基线恢复错误处理和取消有 DL2-003/004/029 缺口；分支修复不等于已集成 | Host 恢复与能力协商独立验收 | AppleCore 修改还须 iOS 回归 |

主要源码定位：`apps/windows-ui/src/controller.ts` 与 `src-tauri/src/controller.rs`、`apps/windows/src/runtime.rs`；`apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift::consume`；`apps/macos/Sources/DeskLinkApp/Views/SessionView.swift`、`Input/SessionInputView.swift`、`Bridge/HostBridge.swift`、`Capture/MacH264Encoder.swift`。Mac 编码器在基线已有真实 VideoToolbox 实现，不能再写成未来空白计划；它与协商 Main/1080p 的声明漂移是独立 DL2-043 问题。

## 初始工程验证与历史记录（已由第一波报告承接）

工程切片的 Windows 本地结果：三个 Bun 项目测试 246 / 20 / 14 项与类型检查、前端生产构建、Python 127 项、Rust fmt/Clippy/workspace lib、协议与会话契约、localhost transport/relay、FFI runtime、媒体恢复及 Windows runtime 契约通过。原始命令/日志见本工作区 `dist/verify/verified-local.json`；记录的 source_commit 为 `a905b88`、dirty=true，表示基线加当时工程切片，而非 main 已整合所有修复。后续文档元数据检查另有独立结果。

初始工程的 `native perf` 当时在 Windows 环境记录 Apple、未授权交互设备、未集成离线基线和完整性能条件为 skipped，报告 incomplete / passed=false。未运行真实远控、输入注入、线上发布/探针、Swift/Xcode、双机、GPU/CPU 对照或长稳。入口说明见[分层本地验证](local-verification.md)。2026-07-26 的 Apple、旧 Windows 发布冻结和其他历史报告保留原范围；第二阶段以 [TODO](../TODO.md) 前九节及本矩阵为准。

5501aad历史合同过渡报告 `dist/verify/input-context-transition.json` 记录源码 `5501aad`、dirty=true：workspace 全target/feature编译、protocol **47项**、crypto/transport79项通过；host runtime 9通过/1失败，生产 `HostVideoPipeline::prepare` 的零epoch被拒绝为 `Protocol(InvalidVideoConfig)`，报告整体 failed / passed=false。来源分支报告49项，本工程实际执行47项，计数不能照搬。它是待owner接线的业务失败，没有 skip、填默认值或改断言标绿。此前 `probe-integration.json` / `probe-clippy.json` 的源码是 `4c3648a`，仅证明此前 probe 接入，不能证明新过渡源码可运行。

API桥接后的 `dist/verify/captured-context-bridge.json` 记录干净源码 `56f53b4`、dirty=false：编译、protocol47项及 controller加密loopback5项通过（0忽略）；host runtime仍9通过/1失败，整体failed / passed=false。这是新API原语通过，不是七处生产调用、完整C/Swift/FFI、host epoch或实际呈现已接通；DL2-013严格绑定继续未完成。

有序Sender后的 `dist/verify/ordered-input-sender.json` 记录干净源码 `d6f153e`、dirty=false：fmt、编译、protocol47、独立controller-loopback6通过；完整ffi-runtime内lib12、ABI7、platform1、controller6、directory4通过，但Host8通过/2失败（旧配置epoch0），所以完整FFI失败；Windows host仍9通过/1失败。此失败仅属于尚未整合完整Apple ABI/Host依赖的本工程旧源码，不能外推为Apple当前源分支失败。加密回归实际覆盖32并发clone和Runtime共36帧连续序号、原context保留、零context不发包/不消耗序号，仍不证明生产迁移、新Noise随机epoch或IPC大整数已完成。整体报告failed / passed=false，不以选中项通过替代完整结果。

## 早期独立分支的历史证据快照

下表只记录指定旧commit及其当时TODO/测试证据，不自动包含后继提交。当前第一波已经合入下方更新的完整来源并独立实跑；这些早期来源计数不替代当前报告，main仍未合入。

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
| 046 | 初始取消树已由3277c2a实现，本工程已有9c41195等价提交；冻结媒体65c的交接另将165fcca路径所有权/迟到结果仲裁切片列为主项完成 | 当前a4已随J完整引入后续切片并通过软件门禁；下方65c来源报告仍绑定旧源，不与候选计数相加 |
| 048 | key经Noise message3 payload认证解密；未直接纳入Ed签名不足以证明已确认P1/网络替换或UKS路径 | 熵源/派生防御纵深与篡改/拼接测试待评估；不据此直接改crypto或背书新缺陷严重性 |

上述分类没有新增真实远控/性能证明，也没有修改其他编号完成状态；其他新项继续按owner复核当前分支后逐项记录。

## 初始化保留缺口：源修复与Windows joint回归已记录

协调方joint只读审查与本工程固定媒体 `89bd1e5161653dd668d290a417bd5f6cc146290a` 源码核对确认过DL2-021初始化缺口：打开已有resume partial后，ready发送失败直接return，未走preserve，IncomingFile的remove_on_drop=true导致partial被删。媒体已独立提交 **`65c70fd658d0526a8a27b75fdbf120ae065222bf`** 修复，在同一磁盘线程先preserve后退出；当时本工程只读Git确认parent为 `6dbe53d64c60bbdd50af4b592ebef7471b800fcc`，仅TODO+transfer变更。源TODO报告transfer28项通过，后续joint5782341已保留65c历史；其原日志明确包含初始化取消保留、空part/显式cancel、初始化错误和sync失败四项通过。当时本工程仅核对固定源码/日志。当前第一波已完整合入该历史并通过软件门禁；原来源与局部联合回归计数继续分别保留；详细链路见[证据记录](second-stage-review-evidence.md)。

此前协调方报告的joint未提交/索引UU/测试未跟踪，是当时的审查快照；本工程未检查其WIP，不能将该快照当作当前状态。其日志计数已更正为shared174通过/1忽略（原274笔误撤回），不是本工程实际测试，也不能替代最终jointSHA/报告。除此项之外“暂未见新确定缺陷”的只读review表述不等于测试通过。FFI Mac Host/Swift、实际WebView和双机仍独立；本工程原始失败报告继续按各自源码身份保留。

## 联合收口的来源报告（计数仍绑定各自旧源）

| 来源 | 已核对或收到的证据 | 当前边界 |
| --- | --- | --- |
| 产品 `27fec756ba97ee6f67228dc9530c7b0578a49ff3` | 本工程只读Git对象确认其parent为 `16594c4f8780cd565afd57695f88e29277ec5939`；代码仅将既有owned future提取为 `InputSendFuture<Writer>` 类型别名，发送语义不变。提交TODO报告UI库135通过/1项签名资产忽略、本包 `--lib --tests --no-deps` Clippy通过 | 当时仅核对来源；本包Clippy不代表跨包严格检查。当前已随J完整保留27fec历史并另有候选实跑 |
| Apple协议13冻结 | `f954a68153168a1ded048ddf93eb9c565684d096`，tree `eed9ea733f256024f5839ce0a7c6763d3a6a920a`，41笔完整历史；协调方核对来源原生165通过/0忽略（FFI88+transport/relay77）、Python201及限定check/Clippy/双头/fmt | 旧71/186报告保留历史身份；当前已完整合入f954与cb7514，保留J真实Windows非零producer，候选FFI91另有实跑。源报告不替代Swift/Xcode或真实呈现 |
| Windows最终joint | `5782341e22ba55b2efe30a8eeaeafbebfe0daaba`，tree `59c7270aea76bc1d542ac49c625e308b42ef20f2`，parents27fec+65c；27笔产品/37笔媒体历史、已跟踪联合测试与477源码blob已核对 | Rust634通过/2失败/19忽略，Cargo101；八包严格Clippy与Bun474/构建通过。完整Apple尚未合入，两项Host零epoch失败保留，整体验收继续未完成 |

原产品16594与媒体89bd的合并安排、旧joint审查和工程d6f153e失败报告均保留其历史身份。joint578已有021局部回归和013本地加密软件链证据；来源报告本身不关闭041/013；当前已完整接通Apple来源并通过第一波软件门禁，真实平台与设备验收仍未关闭。

## 媒体源分支冻结交接（65c70fd）

本工程只读核对 `E:/desk-link/.worktrees/dl2-targets/windows-media/validation/final-validation.json` 与同目录 `windows-media-handoff.json`。验证manifest的source_revision/source_revision_after均为 `65c70fd658d0526a8a27b75fdbf120ae065222bf`，working_status为空；协调方另已核对该源HEAD、干净工作树与Cargo.lock哈希。冻结交接包含37笔提交，完整来源清单留在handoff，不按整文件覆盖或丢失逐问题历史。

来源测试分三组为266通过/17忽略、177通过/1忽略、FFI controller_runtime 5通过/0忽略，合计 **448通过、0失败、18忽略**；manifest另记录workspace/all-targets check、指定媒体/共享包lib/tests严格Clippy与workspace fmt通过。它没有声明完整FFI、Bun/前端/安装包、真实远控或原生设备全部通过，18项忽略保留为未运行。完整计数及哈希核对见[审查证据记录](second-stage-review-evidence.md)。这些是媒体源范围结果，本工程当时未复跑该来源；当前a4完整候选另有实际软件报告，不借用这些旧源计数。

| 交接主项状态 | 编号 / 已实现范围 | 仍需保留的边界 |
| --- | --- | --- |
| 完成（仅三项） | 020：可靠视频builder增量构造/复用buffer；046：同peer首接纳路径所有权、按Arc身份退役和迟到结果取消；050：重复视频chunk保留首次payload且不延长生命 | 仅对应65c源分支交接状态；不直接修改本工程或joint的完成状态 |
| 部分实现 | 015：复用staging纹理与NV12 scratch | 仍有CPU读回；GPU缩放、NV12 surface、MFT设备绑定与租借池未实现，不能称GPU surface链完成 |
| 部分实现 | 030：单次流式源hash、Windows只读snapshot、peer退出取消 | 仍限256MiB；大文件/4GiB、目录与持久块证明未实现 |
| 切片完成、主项未完成 | 010、011、012、013、014、016、021、022、023、044、045 | 各自pending仍按handoff保留：生产者/呈现/产品接线、真实硬件成功编码、GPU/双机性能、慢盘/驱动/睡眠恢复或实际诊断等不能由源码和合成测试代替 |

041与013整体平台验收保持未完成；早期工程过渡源码现已由第一波完整来源整合取代，软件检查和本地签名/store闭环已通过，真实设备与发布继续独立验收。

## 当前第一波与后续波次

第一波协议13已在a4完成软件收口：Windows joint578、Apple完整历史f954及cb7514后继、网络4b、工程6b、GUI506、producer28与服务d2完整merge并实际验证，保留逐问题历史。main合入由主会话决定；根目录未知044–061未提交稿继续保留，工程组未写入且owner尚不明。

GPU015/016在 `codex/dl2-windows-gpu` 已冻结后续来源 `23526f23f450e64edea098a36b07ed4c62d4982b`，7笔完整历史。来源报告为275项默认通过及15项显式GPU用例、17项仍忽略；合成矩阵在6a60执行，235仅文档后继。上述仍是owner来源结果；本工程另在97ff新集成候选实际复跑25软件检查和24选定合成GPU用例，原6a60性能矩阵未重跑。GPU代码与合成硬件链已进入第二波，第一波a4不含它们；真实捕获、质量等价、端到端性能与设备恢复仍待验收。

文件030由第七会话 `01a17733-4bf0-79d3-831f-7c0190191591`、`codex/dl2-file-transfer` 从joint578负责，独占新增文件业务协议14；当前工程/Windows joint仍13、main12。GPU和文件后续开发不阻塞稳定13第一波。

044/045云诊断闭环必须合入服务固定schema历史：044为b1118ad，045为 `bf5d7eb27f3f41be59ac317b63d7e93d10f38066`。GUI云过滤WIP已由协调方确认全部撤回，后续正常上传；当前已用a4新生成的原生签名fixture完成19项本地HTTP/schema/SQLite重开、去重及分析用例，不能以旧joint缺schema为由永久吞掉事件。

GUI044已独立提交 `67cd1bf69cbeacf2a761054f17454f40e4b821f1`，直接parent为joint578，本工程Git核对cloud_diagnostics.rs相对578无diff；协调方报告cursor fixture逐字节对应b111服务合同。owner所选Windows218通过/2忽略、UI137通过/1忽略及两包lib/tests Clippy为来源报告，该旧来源计数不覆盖joint634/2/19；当前a4已有独立实际跨包报告。GUI045已随506完整历史合入，producer28与服务d2亦已合入；当前以第一波实际19案例闭环为本地软件证据，保留设备/远端送达边界。
