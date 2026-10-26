# 第二阶段所有权、错误与资源合同

更新日期：2026-10-26。初始实现核对基线为 `a905b88`，工程验证/文档提交为 `44a2a99`、`376abb2`；第一波已测试源码为 `a4c5b404d43794d646394923a481f23c9298c3ed`。DL2-040 目前已实际完成共享 transport 错误分类的职责拆分，其余具体 owner 迁移逐项待办；**没有完成巨型 controller/runtime/FFI 模块整体拆分，也没有性能收益结论**。当前能力、完整合并来源与候选验证见[能力矩阵](current-capabilities.md)及[当前架构契约](current-architecture-contract.md)。

第一波已完整合入 J578、Apple f954及CBcb7514、网络4b、工程6b、GUI506、producer28与Sd2，并通过 v3 的25个软件门禁。它尚未合入协议12的 main；Swift/Xcode、实际 WebView、输入注入、双机/拓扑、完整 Mac 功能和硬件/性能门禁继续 `[ ]`。以下初始表格及5501/56/d6过渡报告保留各自历史身份，不再表示当前候选未合入 Apple 或仍有零 producer。

后继GPU候选已在独立worktree从第一波冻结c3合入235完整七笔历史：干净 `97ff7c08199cdad4511b0044ddf783d79e2295b1` 的25软件检查、24选定合成GPU用例和19签名/store案例通过。实际合并保留捕获前context、按frame/config查回原输出身份及Host/Controller分类owner；native/perf仍2通过7跳过，040剩余职责拆分/性能未关闭。详见[第二波GPU报告](wave2-gpu-integration.md)。

## 初始源码核对（main@a905b88；历史）

初始核对先用主 checkout 的 CodeGraph 查询 `SessionMachine`、`DesklinkResult`、`transport_error_is_retryable`、调用方与影响范围，再读取当时 worktree 源码确认。索引不是后续改动后的最终事实；下表记录当时的所有者与分散策略，当前共享错误分类的已落地切片见文末。

| 边界 | 当时已有所有者/符号 | 当时分散的策略 |
| --- | --- | --- |
| 会话状态与动作 | `crates/desklink-session/src/state.rs::SessionMachine::apply`、`SessionAction` | 调用方仍独立决定何时把错误提交为 `Disconnected { retryable }`。FFI `DesklinkRuntime::advance` 基线只消费 BeginStream，ReleaseAll/Close 还有独立路径，不能宣称所有动作已由此唯一执行 |
| 重连预算 | `crates/desklink-session/src/reconnect.rs::ReconnectSchedule::next` | schedule 只返回延迟/耗尽/过期。Windows `schedule_failure` 对 persistent 耗尽后继续，FFI `schedule_retry` 可结束；Windows 稳定连接 30 秒与 FFI 收到 VideoConfig 即 stable 的预算重置定义也不同 |
| 传输错误 | `crates/desklink-transport/src/lib.rs::TransportError` | FFI `worker.rs::transport_error_is_retryable`、Windows host `runtime.rs::transport_error_is_retryable`、Windows controller 的目录分类/`ConnectFailure` 各自判断。QUIC 已有 PendingReliableSend 在部分可靠发送被取消时关连接，不能写成完全没有取消安全防护 |
| 控制端错误 | `crates/desklink-ffi/src/controller.rs::ControllerError` | FFI `worker.rs::controller_error_is_retryable` 与 Windows `ConnectFailure::from_controller/from_transport` 重复分类；Windows 根据 persistent 区分 SessionNotFound，FFI 基线未带相同信息 |
| Host 生命周期 | Windows `host_error_is_retryable_for_session`、FFI `host_worker.rs::is_retryable_host_error` | Windows 把恶意/中断 peer 的失败限制到当前 attempt，并允许 host 服务重建；这不等于控制端应重试同一个认证失败 |
| C ABI 与 Swift | `crates/desklink-ffi/src/lib.rs::DesklinkResult/DesklinkState/DesklinkEventKind`、两份 `desklink.h`、Apple `ControllerBridge.consume/consumeState` | Rust 内部有类型，跨 ABI 使用固定数字；Swift 基线事件仍有裸数字 case。基线 DesklinkResult 只有 Ok/InvalidArgument/InvalidUtf8/InvalidState/InternalError，不能假称已有完整 Busy/Cancelled/Closed 区分 |
| 停止与句柄 | FFI `ControllerWorker::shutdown/Drop`、`DesklinkRuntime::stop_worker` | 基线会发送取消再同步 join；Swift 错误路径会调用 disconnect。Apple 分支 003/004 已有修复，但本基线尚未整合，不能据此声称 UI 停止已完全有界 |

Windows 的 persistent 会话、临时邀请、host 服务和单次 controller attempt 是不同生命周期。目标是由共享策略在显式上下文中作唯一判断，而不是要求四种对象对同一底层错误返回同一个布尔值。

## 收口时共同遵守的合同

以下是 DL2-040 后续迁移的验收规则，尚未被所有生产路径统一执行。每次纵向切片须使生产调用和回归调用进入同一所有者，并删除被替代分支。

| 所有者 | 资源与取消合同 | 错误/状态合同 |
| --- | --- | --- |
| protocol | 有界解析、显式长度/版本/能力；不持平台资源、不执行 I/O | 定义 wire 类型和稳定原因；不通过用户文案推导重试。postcard variant 顺序不可任意重排 |
| crypto | 密钥与认证上下文归共享层；短临界区，不跨网络 I/O 持加密锁 | 认证失败、会话代数和完整性错误保持类型，不由平台字符串覆盖安全判断 |
| session / 共享恢复策略 | 纯规则输入包含角色、阶段、授权/会话寿命和代数；平台执行动作 | 明确重试当前 attempt、重建 host 服务、回退媒体路径、等待用户或终止的不同范围；一次错误不能同时归两个 owner 决策 |
| transport | 每条 lane 的队列与任务有界；关闭/取消唤醒等待；慢盘与无关媒体不占输入读循环 | 连接、目录、容量、协议错误保持稳定类别；视频 direct 回退保留控制会话，不能把数据面失败升级为整个会话退出 |
| runtime / FFI | 接纳命令、完成操作、发终态三个阶段分开；停止先撤销新接纳/释放输入，再取消任务并等待资源；句柄/callback context 生命周期跨线程明确 | Ok 只承诺相应 API 的接纳含义，不能等同已发送/已注入/文件完成；背压、取消、已关闭、参数错误不能都丢成 InternalError；取消不能由普通队列满而失效 |
| platform adapter | 拥有捕获、编码、原生注入、文件 I/O、权限和 Keychain/DPAPI；回调借用数据在跨 actor/线程前取得明确所有权；UI 主线程不等待潜在阻塞 join | 把 OS 失败归为能力/权限/瞬时资源等类型，保留恢复范围；不复制 wire/schedule；不把不支持的命令静默记成功 |
| UI session store / presenter / input / transfer / shell | worker 代数、stream 与目标身份约束迟到结果；真正离开会话释放输入/展示，普通 shell 更新保留会话表面 | 用户文案只呈现共享层结果与下一步，不用文案/裸整数重新猜终态；账号、文件目标、审批和媒体资源各有明确 owner |

### 错误结果的共同语义

- 命令接纳错误、可恢复通知、终态和资源已释放确认分别表达。`Error` 事件本身不意味着句柄必须销毁；是否可恢复由稳定类别和会话状态决定，不能只看到非空 message 就 disconnect。
- 恢复判断至少保留错误来源、发生阶段、作用范围和当前代数。用户主动取消是用户意图；认证/协议不匹配是安全终态；瞬时传输/资源故障可在适用预算内重试。Host 服务隔离失败 peer 与 controller 重试该 peer 不得混为一谈。
- 目录 `DirectoryNotFound` 继续统一“离线或密码错误”，不能泄漏设备存在性。只有密码验证后的版本信息可用于“升级当前电脑/目标电脑”提示。日志用白名单类别、阶段、请求/代数摘要；不把 token、身份材料、文件路径或任意 OS/Provider 字符串放进公共错误。
- 返回 Busy/Cancelled/Closed 等新增值前，Rust 类型、ABI 两份声明、Swift switch 与 TS/Rust IPC 的未知值行为必须同步验证。本文不自行分配新 ABI 数字，也不另起协议版本。
- `InputState` 的协议 13 契约复用产品分支 `9979c6424cf99f704feb212c7700833fce466dae`：Blocked / UnsupportedKey / InvalidInput 与 `None` 恢复；这是非终态输入反馈，不自动关闭画面/会话。该提交在初始a905基线尚未集成，当前候选已随J保留契约、持续失败汇总/恢复和提示接线，真实权限验收另列。

## 集成前置与后续验收

| 边界 | 历史独立提交证据 | 当前候选承接与后续迁移限制 |
| --- | --- | --- |
| 错误与取消 | Apple 003 `30a902ae6a73a2013be2b1fdf667f577ca9263d7`、004 `7ddd70e7293d23bb85f3c13b02fc9f7d4b1a2b21` | 当前已保留完整f954与CB后继的可恢复通知、发送取消和handle retirement；后续抽取保留这些接口，不替平台重造另一套语义，Swift实际行为仍待验 |
| wire / ABI | 产品 009 `9979c6424cf99f704feb212c7700833fce466dae` 已有协议 13；Apple 025 `5691aba250770d3c781c472966263a40ec0018de` 已有生成头、有界命令与 ABI layout 回归 | 当前FFI91通过，含真实MSVC C布局与双头检查；后续同步生成声明，不设计第二套、不从WIP复制或另bump。媒体009 `134356905549a8c27c003222ab1303f985c5a277` 与网络009 `aadf757241b9d0eefa13c4a719153d0ecd0a0a2c` 的历史契约传播本身不算真机反馈验收 |
| 多通道数据面 / UI | 网络 024 `30bef6296e90d02d741375ac888b63f772da4698` 已有多通道 LAN 切片；产品002 `47f429d376290b7c67aebb3965e5ff7f80ec68c2` 已有独立 session surface | 当前J已包含002；024后续平台生产接线不合入第一波。继续沿最终生产调用验证transport关闭/回退与UI生命周期，不基于旧版本再造替代实现 |
| 传输与文件所有权 | 媒体 011 `3277c2a41af0ac923e1daa1c5cab5d952b355b9b`、021 `c2ec1498f8ed62630895abfd868db88ed499e2d4`；网络 019 `e173cf26f975f1cd28efe727d2afa4ce3aa4f02c` | 当前J/N已保留锁、读循环、磁盘worker及控制端接线，含65c初始化取消保留partial；后续每项owner迁移仍须删除被替代分支，不以wrapper掩盖重复owner |
| 性能对照 | 媒体 014 `254f8be3e11473114ed48c71aeffa1693ccbf0bd` 已有离线合成分段基线 | 当前CPU离线基线已通过；native/perf合计2通过7跳过、整体incomplete，不等同完整同硬件、双机、输入到显示与长稳基线，040性能验收仍未完成 |

第一波已取得可重复的软件候选revision；后续按协议/错误合同→取消与资源生命周期→平台 adapter→UI ownership逐切片迁移，不以等待全部功能或平台验收完成为前提。每个切片单独提交并保留关联 DL2 编号与适用回归；禁止以整份分支 TODO 覆盖其他进度。

### 追加所有权：输入 epoch 与探测 lane

2026-10-26 的协调决定：**Apple/FFI 组是严格输入上下文的唯一共享契约 owner**。该组合同 `bc6c0a62d3817fbe44759cddf5069c3dee117f4c` 已复用为本工程 `5501aad9f7d160f751048036ac2a14cf0a315680`，仍在同一未发布协议 13 开发线。`VideoConfig` 在 `stream_id` 后有非零 `input_epoch: u64`；`InputEnvelope` 在 `timestamp_us` 后有非零 `stream_id: u64`、`input_epoch: u64`，共享层已有 `InputContext` 类型。codec 的 `encode_input` / `decode_session_input` 已统一保留前缀 `[0, PROTOCOL_VERSION:u16 little-endian]` 和 exact 解码，拒绝旧包混淆与尾字节；平台/transport 不另拼前缀或 fallback。

**历史工程过渡（5501aad→56f53b4→d6f153e）：** 来源桥接 `b473498382bd163f6085c019521105741264707c` 复用为 `56f53b4a17347714a91a2cb269bc5addc5777916`，`ControllerRuntime::send_input_with_context` 显式校验并保留捕获值、旧send_input拒绝，加密loopback本地5项通过；两文件增量与完整9ad等价。当时尚未引入完整C/FFI ledger/worker，七处生产旧API调用未迁移，FFI `DesklinkRuntime::emit_input`、Windows `HostVideoPipeline::prepare` 与FFI host配置仍用无效零context/epoch，host runtime失败InvalidVideoConfig。d6f153e的Sender原语没有独立解决这些缺口。上述失败保留原始source，不能作为当前候选仍未接线的结论。

当前候选已完整保留J的非零Windows producer/host映射、规范u64 IPC/入队context与绘制门禁，完整f954的FFI/Apple队列、sender、media_generation与呈现接线，以及CB的最终输入admission。FFI91含真实C ABI与Windows joint两项软件MFT/QUIC/Noise联合回归已通过；Swift六项Recording和真实presented/输入/拓扑仍未执行或未完成。禁止默认1、发送时补当前epoch或跳过失败标绿。下表是持续所有权合同，不能用已有本机拓扑切片 `631cb3258d1cc3b53977611a0603f17d685b850c` 或自动验证替代全部平台验收。

| 唯一 owner | 必须交付的边界 | 不能替代它的近似 |
| --- | --- | --- |
| Apple/FFI：共享 wire/ABI | 视频配置和每个输入/远端 release 都带同一明确 stream/epoch 上下文；跨层传递保留该值 | sequence、wallclock、仅 config_version、源码中出现字段名 |
| 媒体：host 注入/映射 | 原子清键、推进 epoch 与映射发布；在同一映射锁语义内核对当前 stream/epoch 再注入，避免检查到使用间切屏；拒绝旧输入和旧 release | 仅稳定显示器 ID、本机拓扑代数、切换期间短暂停输入，或无条件保留旧 release |
| 产品：实际呈现与入队 | 匹配新上下文的首帧实际呈现后再启用该 context 的远端输入；事件入队即固定 context，不能出队时贴成新 epoch | 仅收到 VideoConfig、解码完成但未呈现、把排队旧事件重新标成当前上下文 |
| runtime：取消与本机收回 | 用户取消/Host 本地收回独立于远端输入上下文和普通队列容量生效 | 用输入 epoch 过期或输入队列满阻止取消/本机清键 |

第一波同一revision已完成共享合同、host和呈现/入队代码的聚合及适用软件回归；DL2-013整体验收仍 `[ ]`，缺项包括实际WebView、Swift/Xcode/Recording/真实presented、输入注入、双机与完整Apple同stream源拓扑映射。持续保留“新epoch持键后旧key-up/ReleaseAll不能释放新持键，而用户取消/Host本机收回独立清理”的区分；远端过期release拒绝不能阻止本机取消。后续owner迁移按具体边界开展，不提前重写其他组活跃模块。

### 新认证会话的新鲜度与完整u64合同

源码反证：`HostConnectionSettings.stream_id` 随 `configuration.rs::encode/decode` 保存并恢复；`create_managed_connection` 创建时用字符串 `"1"`，`prepare_host/prepare_pairing` 将持久值传给HostSupervisor；supervisor只在本次run内checked推进。因此“新Noise/进程自然有新stream，epoch可从固定1重新开始”只能视为已撤回的历史假设，不能作为验收条件。

当前协调规则是：每个新认证会话/Noise重新用 **OsRng分配非零随机epoch起点**，同进程新会话也不能复用缓存的base+初值；会话内仅在有效几何、活动输出或输入有效性边界改变时checked单调推进，并与清键/映射更新原子协调。纯码率/profile/GOP等不改变有效输入映射的编码调整保持epoch。新会话的随机epoch可以小于旧会话值，核对的是当前上下文，不靠跨会话数值大小判断新旧。

RNG失败、零值和checked推进溢出必须有明确失败或上下文重建结果；不能默认1、wrap、round/clamp或悄悄沿用旧上下文。旧plaintext即便被重新用新Noise加密，也必须保留事件入队捕获的旧context并被拒绝；不能因为认证会话更新就给旧事件贴新epoch。该随机epoch只做输入新旧绑定，不作为密钥、随机nonce或Noise安全修复。

Windows的context与VideoConfig身份IPC使用规范十进制字符串与BigInt，保存完整u64；C/Swift继续使用uint64_t/UInt64。当前J已包含随机起点、host映射和完整u64 IPC生产接线，A/CB已包含原context及native receipt传递；超过2^53的JSON/IPC、非规范文本/number/溢出拒绝和原context入队保持已有软件回归。不能用Number、round或clamp“修复”精度，也不能由Rust/C ABI及合成呈现通过推导Swift、实际WebView或设备呈现通过。

### epoch与真实帧配置的配对

源码中的 `VideoFrameHeader` 与 Windows `VideoMailboxKey` 只有stream_id/config_version，没有独立frame epoch。当前 `ControllerRuntime::next_event` 已拒绝同stream+同config_version却改变VideoConfig任一字段的配置，返回InconsistentVideoConfig；VideoConfig的Eq包含input_epoch。必须保留这道保护。“同version换epoch使旧帧误激活”是放宽/绕过该拒绝后的设计风险，不能写成当前已复现故障。

Host在同一活动会话内每次input_epoch变化时，必须推进**真实**config_version并发布对应配置/恢复关键帧，即使同尺寸换屏、仅虚拟原点/输入映射或有效性边界变化，SPS/尺寸未变也如此。纯编码/画质可以只推进config_version，保持epoch，不无谓清键。确认token必须来自该帧真实配置；旧buffered编码/解码输出不能在发布时重贴新config_version/epoch。验收同时覆盖旧version变字段明确拒绝，以及正确新version配置和对应帧实际呈现后才启用epoch，不能只测最终尺寸变化。

DL2-011 的独立 lane 来源为 `0836a8b311a858bf12f0d0b4427961c35f5986a7`，经源码核对仅追加 `SecureLane::DirectProbe = 8` 并同步 COUNT/ALL，所有 Exclusive/Shared probe 的 seal/open 迁移；现有 lane 0～7、JOIN 版本与 QUIC 事件槽数量保持原义。慢 probe 必须不被视频重放窗推进挤出，同时继续拒绝重放和跨 lane 解密；当前候选已保留该提交及shared-probe前置并完成统一入口接入验证，不另造探测加密路径。旧来源79项报告仍绑定原source，不替代a4候选报告或main合入证明；双机/候选黑洞性能条件继续未完成。

第一波 `a4c5b40` 已用[统一入口](local-verification.md)完成25个软件门禁，包括共享unit/contract、transport/FFI localhost、媒体恢复、背压/取消及相关平台软件回归。另有19个fresh真实HTTP案例按预期通过，正例直接使用原始RustSignedRequest的body/hash/签名，不以改写或重签替代，覆盖schema、文件SQLite、重开/去重，并保留旧unsafe拒绝与篡改后401负例；诊断范围仅7事件/24个event-field对/18个不同u64键，不扩散字段白名单或把诊断计数当真实丢包/送达。native/perf仍为2通过7跳过、整体incomplete；Swift/Xcode、实际WebView、真实权限和设备缺项保持未验，代码移动或模块行数下降不能作为性能收益。

初始切片只完成现状核对和合同整理。先前工程 `verified-local.json` 已验证基线的 Rust/三个 Bun/Python，`dl2-042-python.json` 为文档元数据回归；二者都不是各组整合后的回归。后续稳定职责拆分逐项记录如下，巨型模块整体拆分和 DL2-014 前后比较继续保持未完成。

## 已落地040切片：传输错误分类的单一所有者

新增 `crates/desklink-transport/src/retry_policy.rs`，由共享层唯一穷尽匹配TransportError/JoinRejectCode；Windows host runtime与FFI controller worker删除原重复match，按 `TransportRetryContext::HostSupervisor` / `ControllerSession` 委托同一函数。CodeGraph导航后读取本工程及固定Apple880e3f87、网络a41e1e2源码，确认两处原分类在这些来源逐字相同；此次抽取不选择某一分支覆盖另一分支语义。

共同的暂时连接/流/数据报错误和容量拒绝仍可重试，认证、角色、协议/参数、目录错误仍永久失败；Malformed、SessionNotFound、SessionOccupied仅让HostSupervisor重新接纳peer，不让ControllerSession重试无效会话。测试覆盖全部25个error/rejection用例的双上下文，以及两个生产包装入口。SessionMachine、persistent/expiry/重试次数/退避、取消与停止决策仍由原所有者执行；该纯函数不做I/O、不读错误字符串、不修改wire或ABI。

历史报告 `dist/verify/transport-policy/report.json` 来自f8a89d6加本切片（dirty=true），Rust1.97.1、Windows Cargo wrapper、隔离target engineering：transport lib29、Windows retry_policy_tests18、FFI lib12全部通过且0忽略；三个相关包lib/tests严格Clippy（无no-deps）及workspace fmt通过。此职责迁移现已随工程历史进入a4第一波并通过联合软件门禁；5501aad/56f53b4/d6f153e的旧Host零epoch与未整合Apple失败仍仅是旧source记录，不覆盖当前FFI91通过结果。平台验收仍独立，未宣称性能提升。

其余具体边界：完整f954与CB已经提供controller input/presentation/media_generation及FFI worker输入发送/admission接线；进一步职责抽取仍按各具体owner单独迁移并保留生产回归，不以等待全部Mac功能或设备验收完成为前提。FFI HostError::Transport(String)仍需独立类型化迁移及Host生命周期回归后才能复用共享策略，不能顺手改成Controller语义。GPU235、LAN024后续生产接线与文件协议14保持后续独立候选，不合入本轮，也不阻塞已完成的共享分类及稳定13软件候选验证。
