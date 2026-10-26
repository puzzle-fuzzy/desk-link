# 第二阶段所有权、错误与资源合同

更新日期：2026-10-26；DL2-040 的不冲突整理切片。实现核对基线为 `a905b88`，工程验证/文档提交为 `44a2a99`、`376abb2`。本文把已存在的边界、待统一的语义和集成条件分开；**没有完成 controller/runtime/FFI 拆分，也没有性能收益结论**。当前能力与独立分支快照见[能力矩阵](current-capabilities.md)。

## 源码核对到的现状

先用主 checkout 的 CodeGraph 查询 `SessionMachine`、`DesklinkResult`、`transport_error_is_retryable`、调用方与影响范围，再读取本 worktree 源码确认。索引不是本分支改动后的最终事实。

| 边界 | 已有所有者/符号 | 目前仍分散的策略 |
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
- `InputState` 的协议 13 契约复用产品分支 `9979c6424cf99f704feb212c7700833fce466dae`：Blocked / UnsupportedKey / InvalidInput 与 `None` 恢复；这是非终态输入反馈，不自动关闭画面/会话。该提交尚未集成到本基线。

## 集成前置与后续验收

| 前置 | 已有独立提交证据 | 对 DL2-040 的限制 |
| --- | --- | --- |
| 错误与取消 | Apple 003 `30a902ae6a73a2013be2b1fdf667f577ca9263d7`、004 `7ddd70e7293d23bb85f3c13b02fc9f7d4b1a2b21` | 先固定可恢复通知、发送取消和 handle retirement 的最终接口；本轮不替 Apple 组重写 FFI/Swift |
| wire / ABI | 产品 009 `9979c6424cf99f704feb212c7700833fce466dae` 已有协议 13；Apple 025 `5691aba250770d3c781c472966263a40ec0018de` 已有生成头、有界命令与 ABI layout 回归 | 先核对该生成声明与003错误布局，不设计第二套；只复用独立契约，不从 WIP 复制协议/ABI、不另 bump。媒体009 `134356905549a8c27c003222ab1303f985c5a277` 与网络009 `aadf757241b9d0eefa13c4a719153d0ecd0a0a2c` 的契约传播也不能单独算端到端反馈完成 |
| 多通道数据面 / UI | 网络 024 `30bef6296e90d02d741375ac888b63f772da4698` 已有多通道 LAN 切片；产品002 `47f429d376290b7c67aebb3965e5ff7f80ec68c2` 已有独立 session surface | 两者尚未在本基线整合；transport 关闭/回退与 UI 生命周期须沿最终生产调用验证，不基于旧版本再造替代实现 |
| 传输与文件所有权 | 媒体 011 `3277c2a41af0ac923e1daa1c5cab5d952b355b9b`、021 `c2ec1498f8ed62630895abfd868db88ed499e2d4`；网络 019 `e173cf26f975f1cd28efe727d2afa4ce3aa4f02c` | 先确认锁、读循环、磁盘 worker 与控制端接线的集成版本，再迁移公共 policy，不能用另一套 wrapper 掩盖重复 owner |
| 性能对照 | 媒体 014 `254f8be3e11473114ed48c71aeffa1693ccbf0bd` 已有离线合成分段基线 | 尚未在本分支集成运行；不等同完整同硬件、双机、输入到显示与长稳基线，不能关闭040性能验收 |

后续应先得到可重复的集成 revision，按协议/错误合同→取消与资源生命周期→平台 adapter→UI ownership 逐切片迁移；每个切片单独提交并保留关联 DL2 编号。禁止以整份分支 TODO 覆盖其他进度。

### 追加所有权：输入 epoch 与探测 lane

2026-10-26 的协调决定：**Apple/FFI 组是严格输入上下文的唯一共享契约 owner**。该组合同 `bc6c0a62d3817fbe44759cddf5069c3dee117f4c` 已复用为本工程 `5501aad9f7d160f751048036ac2a14cf0a315680`，仍在同一未发布协议 13 开发线。`VideoConfig` 在 `stream_id` 后有非零 `input_epoch: u64`；`InputEnvelope` 在 `timestamp_us` 后有非零 `stream_id: u64`、`input_epoch: u64`，共享层已有 `InputContext` 类型。codec 的 `encode_input` / `decode_session_input` 已统一保留前缀 `[0, PROTOCOL_VERSION:u16 little-endian]` 和 exact 解码，拒绝旧包混淆与尾字节；平台/transport 不另拼前缀或 fallback。

**wire/codec 与最小 Rust 显式发送 API 已落地，当前仍不可交付。** 来源桥接 `b473498382bd163f6085c019521105741264707c` 已复用为 `56f53b4a17347714a91a2cb269bc5addc5777916`：`ControllerRuntime::send_input_with_context` 显式校验并保留捕获值，旧send_input继续拒绝；加密loopback本地5项通过。该两文件增量与完整9ad等价，后续Apple整合须去重，不包含完整C/FFI ledger或worker接线。七处生产旧API调用仍未迁移；FFI `DesklinkRuntime::emit_input` 仍填零；Windows `HostVideoPipeline::prepare` 与FFI host配置仍填epoch=0，host runtime继续失败InvalidVideoConfig。禁止默认1、发送时补当前epoch或跳过失败标绿。媒体host映射、两平台生产者、队列context与实际呈现门禁仍待owner接线；已有本机拓扑切片 `631cb3258d1cc3b53977611a0603f17d685b850c` 不等于端到端绑定完成。下表仍是未全部完成的所有权合同。

| 唯一 owner | 必须交付的边界 | 不能替代它的近似 |
| --- | --- | --- |
| Apple/FFI：共享 wire/ABI | 视频配置和每个输入/远端 release 都带同一明确 stream/epoch 上下文；跨层传递保留该值 | sequence、wallclock、仅 config_version、源码中出现字段名 |
| 媒体：host 注入/映射 | 原子清键、推进 epoch 与映射发布；在同一映射锁语义内核对当前 stream/epoch 再注入，避免检查到使用间切屏；拒绝旧输入和旧 release | 仅稳定显示器 ID、本机拓扑代数、切换期间短暂停输入，或无条件保留旧 release |
| 产品：实际呈现与入队 | 匹配新上下文的首帧实际呈现后再启用该 context 的远端输入；事件入队即固定 context，不能出队时贴成新 epoch | 仅收到 VideoConfig、解码完成但未呈现、把排队旧事件重新标成当前上下文 |
| runtime：取消与本机收回 | 用户取消/Host 本地收回独立于远端输入上下文和普通队列容量生效 | 用输入 epoch 过期或输入队列满阻止取消/本机清键 |

合同、host 和呈现/入队三段尚未全部接通并在同一 revision 通过前，DL2-013 的严格绑定保持 `[ ]`；特别验证新 epoch 持键后收到旧 key-up/ReleaseAll 不影响新持键，而当前用户取消仍能清理退出。工程组只维护这些门禁，不提前重写活跃的 runtime/controller/FFI。

### 新认证会话的新鲜度与完整u64合同

源码反证：`HostConnectionSettings.stream_id` 随 `configuration.rs::encode/decode` 保存并恢复；`create_managed_connection` 创建时用字符串 `"1"`，`prepare_host/prepare_pairing` 将持久值传给HostSupervisor；supervisor只在本次run内checked推进。因此“新Noise/进程自然有新stream，epoch可从固定1重新开始”只能视为已撤回的历史假设，不能作为验收条件。

当前协调规则是：每个新认证会话/Noise重新用 **OsRng分配非零随机epoch起点**，同进程新会话也不能复用缓存的base+初值；会话内仅在有效几何、活动输出或输入有效性边界改变时checked单调推进，并与清键/映射更新原子协调。纯码率/profile/GOP等不改变有效输入映射的编码调整保持epoch。新会话的随机epoch可以小于旧会话值，核对的是当前上下文，不靠跨会话数值大小判断新旧。

RNG失败、零值和checked推进溢出必须有明确失败或上下文重建结果；不能默认1、wrap、round/clamp或悄悄沿用旧上下文。旧plaintext即便被重新用新Noise加密，也必须保留事件入队捕获的旧context并被拒绝；不能因为认证会话更新就给旧事件贴新epoch。该随机epoch只做输入新旧绑定，不作为密钥、随机nonce或Noise安全修复。

Windows的context与VideoConfig身份IPC使用规范十进制字符串与BigInt，保存完整u64；C/Swift继续使用uint64_t/UInt64。需覆盖超过2^53的真实JSON/IPC往返、非规范文本/number/溢出拒绝与原context入队保持，不能用Number、round或clamp“修复”精度。这些是待owner接线规则，当前sender原语通过不表示随机起点、host映射、IPC或实际呈现已实现。

### epoch与真实帧配置的配对

源码中的 `VideoFrameHeader` 与 Windows `VideoMailboxKey` 只有stream_id/config_version，没有独立frame epoch。当前 `ControllerRuntime::next_event` 已拒绝同stream+同config_version却改变VideoConfig任一字段的配置，返回InconsistentVideoConfig；VideoConfig的Eq包含input_epoch。必须保留这道保护。“同version换epoch使旧帧误激活”是放宽/绕过该拒绝后的设计风险，不能写成当前已复现故障。

Host在同一活动会话内每次input_epoch变化时，必须推进**真实**config_version并发布对应配置/恢复关键帧，即使同尺寸换屏、仅虚拟原点/输入映射或有效性边界变化，SPS/尺寸未变也如此。纯编码/画质可以只推进config_version，保持epoch，不无谓清键。确认token必须来自该帧真实配置；旧buffered编码/解码输出不能在发布时重贴新config_version/epoch。验收同时覆盖旧version变字段明确拒绝，以及正确新version配置和对应帧实际呈现后才启用epoch，不能只测最终尺寸变化。

DL2-011 的独立 lane 已有提交 `0836a8b311a858bf12f0d0b4427961c35f5986a7`，经源码核对仅追加 `SecureLane::DirectProbe = 8` 并同步 COUNT/ALL，所有 Exclusive/Shared probe 的 seal/open 迁移；现有 lane 0～7、JOIN 版本与 QUIC 事件槽数量保持原义。慢 probe 必须不被视频重放窗推进挤出，同时继续拒绝重放和跨 lane 解密；复用该独立提交及所需 shared-probe 前置，不另造探测加密路径。分支提交和其报告的 79 项测试不是本工程接入或 main 整合证明，接入验证须另记源码身份；双机/候选黑洞性能条件继续未完成。

合并后用[统一入口](local-verification.md)验证共享 unit/contract、transport/FFI localhost、媒体恢复和平台 native；特别补背压中停止、迟到回调/代数、持续输入受限与恢复、普通 shell 更新存活、慢盘/网络黑洞。Apple Swift/Xcode、真实权限和设备缺条件时明确 skipped；代码移动或模块行数下降不能作为性能收益。

本切片只完成现状核对和合同整理。先前工程 `verified-local.json` 已验证基线的 Rust/三个 Bun/Python，`dl2-042-python.json` 为文档元数据回归；二者都不是各组整合后的回归。巨型模块拆分、生产策略唯一所有者、Windows/Apple 同语义和 DL2-014 前后比较继续保持未完成。
