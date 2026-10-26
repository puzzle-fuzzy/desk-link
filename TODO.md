# DeskLink 第二阶段 TODO

> 更新日期：2026-10-26（Asia/Shanghai）
> 审查基线：`main@85bdf7278baf11b1adb9b401b978643c261daf2b`；Windows 客户端 `0.1.93`，协议 `12`。
> 本阶段目标：补齐远控工作流、消除真实缺陷、重整模块职责，并以可重复测量推进低延迟、低资源占用和稳定吞吐。允许调整协议、ABI、模块边界及实现，不以开发成本或现有部署兼容性限制方案。
> 用户已确认 Windows → Windows、macOS → Windows 可以正常连接并良好控制，这两条路径是已成立的产品基础。本文件不把旧文档中“尚未验收”的历史表述当作当前不可用证据。
> 本清单已进入并行实施阶段。各项按下述会话分工推进，完成一个问题并通过对应验证后立即独立 commit，同时更新该项完成标记。第一阶段记录保留在文末，不能用其旧日期、版本或勾选状态代替本阶段验收。

## 1. 如何使用这份清单

证据分为四类：

- **复现缺陷**：本次使用真实模块和本地合成输入复现，不涉及线上服务或真实用户数据。
- **源码缺陷**：沿真实调用链确认存在错误分支、契约冲突或生命周期问题，尚未做对应双机复现。
- **能力缺口**：代码明确没有接通或存在硬性限制；属于需要补齐的功能，不代表基础远控不可用。
- **性能结构**：源码证明存在复制、分配、锁等待或调度开销；收益幅度、当前 CPU 占用及用户感知延迟必须实测。

优先级：**P1** 为影响输入、恢复、控制权或核心性能的优先工作；**P2** 为完整体验、持续性能及工程收口；**P3** 为扩展角色。未发现足以在本次证据范围内直接判定为 P0 的事项。

每项关闭时必须补充修复提交、相关测试与真实设备结果。涉及硬件、权限、网络、音频和安装的条目，单元测试或构建通过只关闭对应自动验证部分。实测目标是未来验收标准，不是当前产品测量结果。

| 工作面 | 任务 | 主要结果 |
| --- | --- | --- |
| 核心正确性、输入与恢复 | DL2-001～013 | 输入完整、取消有界、重连不中断、主机可立即收回控制 |
| 性能与数据通道 | DL2-014～024 | 可测量的媒体链路、减少复制、隔离慢路径、低延迟调度 |
| 功能完备与用户流程 | DL2-025～035 | Mac 功能接通、设备可辨识、文件与账号操作闭环 |
| 服务端与长期运行 | DL2-036～039 | 并发注册正确、健康状态可信、存储与邮件可持续运行 |
| 代码整理与验证 | DL2-040～042 | 统一契约、拆解职责、自动验证覆盖真实边界 |
| 扩展角色 | DL2-043 | 被控 Mac 的能力协商与编码配置一致 |

### 并行分工与提交规则（2026-10-26）

| 会话方向 | 负责条目 | 分支 | 首批执行 |
| --- | --- | --- | --- |
| Windows 产品与交互 | 001、002、008、009、017、031、032、033、034、035 | `codex/dl2-windows-product` | 001、034、032，再处理会话生命周期和共享控制 |
| Apple 与 FFI | 003、004、005、006、007、018、025、026、027、028、029、043 | `codex/dl2-apple-ffi` | 003、005、029，再完善 ABI、输入法和会话能力 |
| Windows 媒体与文件 | 010、011、012、013、014、015、016、020、021、022、023、030 | `codex/dl2-windows-media` | 010、011、012、020，以及性能基线 |
| 中继与直连 | 019、024 | `codex/dl2-network` | 019 调度隔离，024 多通道直连后置 |
| 账号与诊断服务 | 036、037、038、039 | `codex/dl2-services` | 036、037、039，再处理邮件与长期运行 |
| 工程验证与收口 | 040、041、042 | `codex/dl2-engineering` | 041 验证入口；040 跨模块重构待共享接口稳定 |

- 各会话在独立 worktree 中工作，不在主 checkout 切换分支或提交别人的改动。表中编号均为 DL2 编号。
- **一个问题一个 commit。** 对应实现、必要测试和该项 TODO 状态一起提交，主题包含 DL2-ID；验证通过就提交，不累计多个问题。整体重构中可独立验证的切片也必须标明所属问题。
- **已完成用 `[x]`，未完成用 `[ ]`。** 主任务只有满足其适用验收要求后才勾选；代码、自动验证、真机验收分开记录，允许只勾选已完成部分。缺少平台、设备或性能数据时，已验证代码仍可提交，但主任务保留未完成并注明缺项。
- 每个问题至少补充修复内容、实际验证命令/结果、未验证条件和提交主题；精确 SHA 在会话交付或后续汇总记录中填写，避免提交正文自引用自身 SHA。
- 先做现有接口内的修复；DL2-004/011 涉及 transport，DL2-009 涉及 Windows runtime，DL2-024/025/030 涉及协议/ABI，变更前检查其他分支已提交的相关修改。先完成接口边界，再开展依赖功能；不并行实施相互替代的协议方案。
- 各会话只维护自己编号的 TODO 条目及必要证据，不改其他任务状态。DL2-040 不提前跨组重写，集成时逐问题保留提交；不把仅在分支完成当成已经合入 main。
- 仅处理本地代码、测试及提交；线上部署、发布和真实远控/输入注入不作为本次安排的自动动作。

## 2. 本次验证基线

以下均为本次在当前 Windows checkout 上执行的结果，不是引用历史记录：

| 验证 | 结果 | 证明范围 |
| --- | --- | --- |
| Windows 前端，Bun 1.3.14 `bun test` | 246 通过，0 失败 | 现有逻辑、契约和合成宿主测试 |
| Windows 前端 `bun run build` | TypeScript 与 Vite 构建通过 | 前端类型及生产构建 |
| account，Bun 1.3.14 | 20 项测试与 typecheck 通过 | 本地账号模块 |
| diagnostics，Bun 1.3.14 | 14 项测试与 typecheck 通过 | 本地诊断接收与分析模块 |
| Python `unittest discover` | 102 项通过 | 现有脚本用例；其中探针重试为测试夹具，不是线上可用性结论 |
| protocol / video / session / transport / crypto | 所选 `--lib --tests --locked` 全部通过 | 五个共享 crate 的本地单元及集成用例 |
| `desklink-ffi --lib` | 12 项通过 | FFI 本地单元测试 |
| `desklink-windows-ui --lib` | 90 项通过，1 项忽略 | 桌面管理层单元测试；忽略项要求显式提供已构建的签名更新包 |

额外的本地复现：

1. 阻塞 `RemoteInputDispatcher` 的首个发送，累计 13 次 `deltaY=100`，实际发送合并滚轮值 **1300**；Rust 协议上限是 **1200**。见 DL2-001。
2. 同一个内存账号库并发提交两次相同邮箱注册，返回 **202 与 500**，后者为 `users.email` 唯一键冲突。见 DL2-036。
3. 关闭账号数据库后调用同一 app 的 `/ready`，仍返回 **200 / ready**。见 DL2-037。
4. 在真实 `DiagnosticStore` 的内存库执行保留期删除的 `EXPLAIN QUERY PLAN`，得到 **SCAN events**。见 DL2-039。

本次没有运行 Swift/Xcode、双机远控、GPU/CPU 性能采样、长稳、真实 UAC/锁屏或公网故障注入。CodeGraph 用于定位符号、依赖与影响范围，结论均再以源码确认。三个专项审查覆盖 Windows 产品流程、原生媒体/传输、Apple/FFI；重连错误与加密锁问题另作交叉复核。

## 3. 核心正确性、输入与恢复

### DL2-001 · P1 · 修复滚轮合并与 Rust 上限冲突

- [ ] 统一跨层输入边界，发送时拆分合并滚轮事件。
- **证据 / 类型：复现缺陷。** `apps/windows-ui/src/remote-input-dispatcher.ts:18,49-57` 将滚轮合并至 4800；`crates/desklink-protocol/src/lib.rs:70` 上限 1200；`apps/windows-ui/src-tauri/src/controller.rs:4656-4663` 拒绝超限。现有 dispatcher 测试反而把 4800 当作正确输出。
- **用户影响：** 快速滚动且 IPC 有背压时，整个合并事件被拒绝，滚动距离丢失；`apps/windows-ui/src/controller.ts:275-277` 的空错误处理令失败不可见。
- **方案：** 由共享契约产生边界常量；累计量和单事件发送上限分离；按合法分片发送并保留方向、总量及按钮/按键顺序。不能用简单 clamp 丢弃剩余滚动量。
- **验收：** 堵塞发送器后累计 5000 单位，每个实际进入 Rust 的事件均不超 1200，合计仍为 5000；覆盖正负方向切换、水平滚动、取消和 stream 切换。

### DL2-002 · P1 · 让远控表面独立于全局页面重绘

- [ ] 将视频、解码器和输入承载改为持久会话表面，普通界面更新只修改局部。
- **证据 / 类型：源码缺陷、性能结构。** `apps/windows-ui/src/main.ts:156-170` 每次 render 都调用 `prepareControllerRender` 并替换整个 app；`apps/windows-ui/src/controller.ts:372-395` 无条件释放输入、关闭解码器及展示资源。打开更多菜单（`apps/windows-ui/src/main.ts:1610-1612`）、本机 Host 状态变化（`1998-1999`）也能触发此路径。
- **用户影响：** 无焦点变化的本机共享事件也会重置连续输入；普通菜单更新会额外引发解码器重建和关键帧请求。菜单真正取得焦点时释放远端按键是必要保护，不能当作缺陷。实际停顿时长未测。
- **方案：** 会话、视频配置、页面可见性分别建模；shell、通知、审批和菜单局部更新；只在真正离开会话、配置代数变化或终态销毁资源。现有账号页离开时的清理要求仍应成立。
- **验收：** 持续输入与静态画面期间，无焦点变化的 shell/Host 更新不重置输入；菜单/审批真正取得焦点时正常 ReleaseAll，但均不因普通刷新关闭解码器或额外请求关键帧；真正退出会话仍清理资源。与 DL2-010 联合验证静态恢复。

### DL2-003 · P1 · 修复 Apple 把可恢复错误当终态

- [ ] 将恢复告警和终态错误分开，保留正在自动重试的 FFI runtime。
- **证据 / 类型：源码缺陷，已交叉复核。** `crates/desklink-ffi/src/worker.rs:558-568` 发送 Reconnecting 后发送 state 仍为 Reconnecting 的 Error；`apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:339-341` 忽略 stateValue，`517-523` 直接 disconnect 并置 failed。
- **用户影响：** 当前代数中的可重试网络故障会由 Swift 主动取消底层恢复。旧代数回调已有过滤，不属于本问题。
- **方案：** 错误事件携带稳定 code、可恢复性和状态；恢复期更新状态及提示，认证拒绝、明确失效或用户断开才销毁句柄。错误文字不再承担状态判断。
- **验收：** 注入 Reconnecting → 可恢复 Error → 新 VideoConfig → Frame，句柄保持有效且恢复画面；终态认证错误才关闭；macOS → Windows 切 Wi-Fi、relay 重启再做真机回归。

### DL2-004 · P1 · 已连接发送背压时仍能及时取消和退出

- [ ] 为发送、关闭及 FFI teardown 建立可取消、有截止时间的异步生命周期。
- **证据 / 类型：源码缺陷。** `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:33,299-305` 在 MainActor 同步 reject；`crates/desklink-ffi/src/lib.rs:620-622,1099-1100` 最终调用 `crates/desklink-ffi/src/worker.rs:223-227` 的 thread.join。`crates/desklink-ffi/src/worker.rs:449-502` 外层 select 有取消，但分支内 send_input / request_keyframe 的 await 没有同等取消保护；`crates/desklink-transport/src/quic.rs:963-1002` 写流仍可能等待。
- **用户影响：** 对端停止读取或可靠流背压时，断开、退出或场景切换可能阻塞 Apple 主线程。
- **方案：** 发送本身参与 cancellation/deadline；关闭 transport 唤醒等待者；将阻塞 join 移出 UI 线程，以停止完成事件管理 callback context 的释放，避免为了快速返回制造回调悬空。
- **验收：** 本地 peer 建连后停止读流、队列满时取消，UI 仍响应；建议关闭完成预算 ≤2 秒，超时有明确清理结果；随后能重新连接。连接阶段超时用例不能替代此项。

### DL2-005 · P1 · 补齐 macOS 字符按键的释放语义

- [ ] 分离物理按键与提交文本，保证按下 / 松开及修饰键状态平衡。
- **证据 / 类型：源码缺陷。** `apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift:15-16` 对普通字符只在 keyDown 发 unicode；`apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:248-252` 只发送 pressed=1；Windows `apps/windows/src/input.rs:359-379` 依赖 pressed=false 生成 keyUp 和修饰键释放。
- **用户影响：** 字符及 Shift/Ctrl/Alt/Meta 组合存在残留按下状态路径，可能要等失焦或断开才整体释放。不能用“文字能显示”证明按键状态完整。
- **方案：** 物理键记录 press/release/repeat；文本提交使用原子文本语义或完整 down/up；flagsChanged 差量同步修饰键；失焦、权限变化和断开统一 ReleaseAll。
- **验收：** 端到端断言所有字符/修饰键事件平衡，覆盖大写、快捷键后输入、拖动、长按、失焦及快速重连，无粘键。

### DL2-006 · P1 · 为 macOS 接入真实本地输入法

- [ ] 接通 AppKit 组合文本、候选、最终提交和文本发送链。
- **证据 / 类型：能力缺口。** `apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift:27,66-67,97-105` 是直接读取 event.characters 的 NSView，没有 NSTextInputClient、interpretKeyEvents、insertText 或 marked-text 管理。现有人工传入“中”的 mapper 测试不覆盖真实输入法。
- **方案：** 使用原生文本承载或实现 NSTextInputClient；组合期间不转发拼音和选词键，提交后有序发送最终文本；候选窗口定位跟随远程输入焦点；与 DL2-005 共用清晰的文本/按键契约。[Apple 文本输入接口](https://developer.apple.com/documentation/appkit/nstextinputclient?language=objc)规定了自定义视图需要接入的组合文本方法。
- **验收：** 真 Mac 拼音、中文标点、中英切换、退格、回车/空格选词、Emoji、死键及多行输入；取消候选不向远端多发 Escape/空格。

### DL2-007 · P1 · 将 Mac 快捷键映射接入实际输入路径

- [ ] 统一“按本机习惯 / 发送原始按键”的策略与生产调用入口。
- **证据 / 类型：源码缺陷。** `apps/macos/Sources/DeskLinkApp/Input/InputMapper.swift:45-53` 有 automatic 的 Command→Control，但 `apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift:97-105` 使用 KeyboardMapper；`apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift:95` 实际把 Command 映为 Meta。remoteModifier 的相关测试只覆盖孤立 helper。
- **用户影响：** Mac 用户常用 Command+C/V/A/Z 控制 Windows 时，实际可能变成 Win 组合键。
- **方案：** 根据已协商的远端平台映射，默认符合用户习惯，原始 Win/Command 模式可选择；会话菜单显示必要系统组合键。不要只修测试 helper。
- **验收：** 断言真实发入 FFI 的事件；Mac → Windows 复制、粘贴、全选、撤销、切换窗口与原始 Win 键均正确。依赖 DL2-005、025。

### DL2-008 · P1 · 被控端能够独立收回控制、暂停和恢复共享

- [ ] 提供“结束当前控制”“暂停共享”“恢复共享”三个独立动作。
- **证据 / 类型：能力缺口。** `apps/windows-ui/src/main.ts:786-793` 共享操作集中在密码；`apps/windows-ui/src-tauri/src/lib.rs:1677-1741` 未暴露停止 Host / 结束当前 Host 会话命令，`1752-1770` 托盘只有打开/退出。`apps/windows-ui/src-tauri/src/host.rs:502` 已有底层 stop 能力。
- **用户影响：** 要保留信任却立即结束本机被控状态，缺少直接入口；退出整个应用还会结束本机作为 Controller 的另一条连接。
- **方案：** Host 与 Controller 生命周期分离；共享页、托盘及被控状态条均能立即收回控制；明确保留信任、临时密码处理及重启后是否继续暂停。暂停不得被账号恢复或电源恢复静默覆盖。
- **验收：** 双角色并行时停止 Host 不影响 Controller；新连接被拒绝，输入与媒体及时停止；恢复共享后保留的信任仍可用；连续点击、重启和睡眠唤醒状态一致。

### DL2-009 · P1 · 让“有画面但无法操作”可解释、可恢复

- [ ] 将持续输入受限转为独立会话状态，并补齐权限场景指引。
- [x] 共享契约切片 `feat(DL2-009): add protocol 13 input restriction state`：唯一业务协议升至 13，末尾追加 InputState，三个稳定原因及 None 恢复；保留 relay JOIN 信封版本。协议/FFI 77 项、目录 6 项、Windows 原生升级提示 1 项测试通过，覆盖非法原因/截断、旧 tag、目录 12↔13 与密码先验、收到旧版 Noise/媒体拒绝。当前仅契约，未宣称输入反馈已接通。
- [ ] 新旧直接/保存连接静默握手的有界退出与提示、持续输入失败汇总/恢复、前端提示及真实权限场景验收。
- **证据 / 类型：源码缺陷、能力边界。** `apps/windows/src/input.rs:306-326` 返回 Blocked；`apps/windows/src/runtime.rs:2714-2723` 把 Blocked / UnsupportedKey / InvalidInput 都转为 Ok。`apps/windows/assets/desklink.manifest:8` 为 asInvoker、uiAccess=false。
- **用户影响：** 输入持续失败时，控制端仍只看到已连接；无法区分权限限制、不支持的键或远端没有响应。
- **方案：** 临时失败保留会话，持续失败按稳定原因汇总反馈并自动解除；对普通桌面、管理员窗口、UAC、安全桌面、锁屏分别定义支持范围；需要提权或受信服务时采用明确的本机授权流程。
- **验收：** 每种权限场景有实机结果；输入受限及时提示可执行的下一步，恢复后自动消除；不得把暂时失败升级为断线风暴，也不能继续静默成功。UAC 可控性本次未实测。

### DL2-010 · P1 · 静态桌面也能响应后续关键帧请求

- [ ] 为恢复请求增加复用最近有效帧或强制快照的产帧路径。
- **证据 / 类型：源码缺陷，已交叉复核。** `apps/windows/src/runtime.rs:1965-1990` 遇到 CaptureOutcome::Idle 直接 continue，`2033-2037` 才处理 force_keyframe；`2537-2542` 请求只设标志。`apps/windows/src/capture.rs:491-499` 的 GDI 兜底只保证首次快照。
- **触发条件：** DXGI 已产生首帧后桌面完全静止，控制端因解码器重建或丢失关键帧要求恢复。周期 GDI 不受同一问题影响；relay 已有可靠关键帧发送，但不能替主机生成新帧。
- **方案：** 请求独立唤醒媒体生产者；以配置版本绑定最近合法帧，限频重编码或强制捕获；不把静态桌面改为常态高频全屏轮询。
- **验收：** 静态桌面主动重置解码器/丢弃 DirectLan 关键帧后，无需移动鼠标或窗口即可恢复；建议请求至显示 ≤1 秒；覆盖与 DL2-002 的组合场景。

### DL2-011 · P1 · DirectLan 探测不能长期占用会话加密锁

- [ ] 将探测网络等待移出共享加密锁的临界区。
- **证据 / 类型：源码缺陷、性能结构，已交叉复核。** `apps/windows/src/runtime.rs:2652-2661` 出站探测持 secure guard 等待 endpoint.connect；`1575-1588` 的各通道 seal/open 共用此锁。`crates/desklink-transport/src/direct_probe.rs:393-431` 内有连接、开流及响应等 await。
- **边界：** 已有并发槽位和逐阶段 3 秒超时，问题不是无限死锁。入站 `apps/windows/src/runtime.rs:2741-2751` 也持锁，但外层 select 可在输入/控制就绪时取消该 future；不能写成入站必然阻塞输入 3 秒。
- **方案：** 短锁生成/验证探测消息，网络 I/O 在锁外执行；必要时派生专用探测上下文，仍绑定已认证会话；停止时取消任务并释放连接。
- **验收：** relay 正常而候选黑洞、慢响应、半包时，测锁持有时间、输入及画面 p95/p99；不能出现由探测锁造成的秒级停顿，探测失败正常回退。

- [x] **已实现并自动验证（`fix(DL2-011): release shared crypto lock during direct probes`）：** transport增加shared探测接口，原有独占接口复用同一状态机；seal/open短锁，连接/开流/读写无锁；Windows出入站均切换，出站任务由JoinSet随peer代数退出取消。
- **验证：** `test -p desklink-transport --lib direct_probe::tests --locked` 8通过；新回环在已建TLS但未响应探测时100ms内仍取得crypto锁并成功加解密Control；入站无流时30ms取消后锁可取得。`test -p desklink-windows --lib direct_video_tests --locked` 2通过（经scripts/run-windows-cargo.py）。
- [ ] **仍待双机测量：** 黑洞、慢响应、半包期间的输入/画面p95/p99和锁持有分布。原实现有阶段超时；入站原select可取消，此修复不把它描述为无限死锁。

- [x] **并发加密跟进（`fix(DL2-011): isolate delayed probes from video replay windows`）：** 短锁后探测不得与视频共用128包重放窗；追加DirectProbe=8独立密钥/计数/窗口，现有lane 0～7不变，所有probe入口统一使用。未另升业务协议版本。
- **验证：** crypto+transport `--lib --tests --locked` 79通过；真实共享endpoint探测等待期间推进256视频包仍完成认证；双向迟到probe、重放与跨lane拒绝均覆盖。修复前共享视频lane的窗口会把这类迟到密文误拒，不能仅把seal移到connect后缩短概率。

### DL2-012 · P1 · 编码器重配与显示切换不能阻塞输入消费

- [ ] 将慢控制编排与输入快路径分开。
- **证据 / 类型：源码缺陷、性能结构。** `apps/windows/src/runtime.rs:2528-2706` 用同一 select 消费 control/input；控制分支等待显示切换（`2552-2560`）、画质切换（`2583-2586`）和自动降档（`2613-2622`）；配置应答等待在 `2476-2502` 最长 5 秒。
- **用户影响：** 编码器繁忙或重建变慢时，切画质和自动调节会让键鼠等待同一循环返回。
- **方案：** 带代数的异步配置请求与完成事件；合并过时请求，单独消费输入；ReleaseAll、取消和断开优先，配置完成不回写旧 stream。
- **验收：** 注入 2～5 秒重配置延迟，键鼠和释放事件仍满足预算；无积压输入突然回放，无旧配置覆盖新选择。

### DL2-013 · P1 · 连接中刷新显示拓扑并稳定识别屏幕

- [ ] 将显示列表、活动屏幕、视频配置和输入坐标作为带版本的拓扑快照。
- **证据 / 类型：源码缺陷。** `apps/windows/src/runtime.rs:1597-1622` 只在会话建立时枚举；`1414-1417` 保存不可变列表，`2545,2565-2571` 按旧列表校验并回发；`1992-2015` 恢复捕获时未刷新列表。`apps/windows/src/capture.rs:387-390` 的 ID 使用枚举序号。
- **用户影响：** 扩展屏热插拔、主屏变化后，新屏不可选，序号变更还可能选择到不同输出。
- **方案：** 监听/检测拓扑变化，用稳定输出身份而非临时索引；活动屏移除时明确回退；旧拓扑下的输入不落入新输出。
- **验收：** 插拔、旋转、主屏切换、100%～200% DPI、负坐标屏幕和睡眠恢复，列表、画面、坐标同步且错误选择可解释。

## 4. 性能与数据通道

### DL2-014 · P1 · 建立第二阶段统一性能基线

- [ ] 先记录真实性能，再按阶段预算优化，保留前后同条件对照。
- **证据 / 类型：工程缺口。** 现有 `apps/windows/src/diagnostics.rs:516` 已记录首帧、FPS、帧间隔、drop 等，`apps/windows-ui/src/video-render-timing.ts` 有滚动统计；本次没有统一的双机 CPU/GPU/内存、输入到显示、各阶段队列年龄与复制量对照结果。现有诊断不等于已经证明端到端性能最优。
- **方案：** 将 capture → convert → encode → queue → transport → assemble → decode → display 与 input → inject 分段；关联同一帧/会话代数，使用单调时钟测本机阶段；跨机测量校准时钟误差，或用外部画面/回显方法，不能直接相减两个系统时钟。
- **验收：** 输出可复跑的硬件/网络/画面矩阵与 p50/p95/p99，记录分配率、copy bytes、功耗/温度、队列年龄、音画延迟和长稳斜率；每次性能变更附同机对照，禁止只报平均 FPS。
- **依赖：** 为 DL2-015～024 提供量化依据；不能为了追求单项 FPS 牺牲输入、文字清晰度或恢复正确性。

### DL2-015 · P1 · 将 Windows 捕获与颜色转换尽量留在 GPU

- [ ] 重构 texture 生命周期，减少每帧 CPU 回读、逐像素转换与临时分配。
- **证据 / 类型：性能结构。** `apps/windows/src/capture.rs:505-521,688-759` 每帧创建 staging texture、CopyResource/Map、分配并复制 BGRA；`apps/windows/src/encoder.rs:122-261,496-501` 再分配 NV12 并 CPU 转色/缩放。
- **方案：** 进程拥有的 GPU texture 池、GPU 缩放/转色与明确 fence/所有权；编码前尽量不回 CPU。必须回退时复用 staging、输出及转换 buffer。不能直接跨 ReleaseFrame 保留桌面复制 surface 并假定仍有效。
- **验收：** 1080p/1440p、集显/独显/混合 GPU 下比较复制字节、分配、CPU/GPU 时间及帧龄；GPU 常驻路径故障可降级且不破坏输入。收益待实测。
- **依据：** [Microsoft 的 Direct3D-aware MFT 说明](https://learn.microsoft.com/en-us/windows/win32/medfound/direct3d-aware-mfts)给出了 surface 输入和 D3D manager 接口；是否能用于具体编码器必须实测能力。

### DL2-016 · P1 · 显式选择硬件编码器并完善降级

- [ ] 建立编码后端能力探测、选择、诊断与恢复机制。
- **证据 / 类型：性能结构、能力缺口。** `apps/windows/src/encoder.rs:734-772` 固定 CoCreateInstance CMSH264EncoderMFT；`1053-1074` 创建 MF buffer 并再次复制 NV12。当前没有硬件 MFT 枚举和 GPU surface 输入链。
- **边界：** 可确认没有显式硬件选择链路，不能仅凭此断言所有设备的实际编码都已测定为纯软件。
- **方案：** 枚举并选择合适的异步硬件 MFT，查询实际能力/低延迟参数，处理设备丢失及软件回退；记录真正使用的后端、profile、分辨率、帧率与原因。[MFTEnumEx 官方说明](https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mftenumex)区分了同步、异步和硬件类别。
- **验收：** Intel/AMD/NVIDIA 与无可用硬件编码器矩阵；同画质对照 CPU、GPU、帧龄和恢复时间，驱动拒绝参数时能够快速选择可用配置。与 DL2-015 联合实现。

### DL2-017 · P2 · 将 Windows 音频播放移出高频 UI 节点调度

- [ ] 用 AudioWorklet 或经测量更合适的原生播放路径，替代逐包节点创建。
- **证据 / 类型：性能结构。** `apps/windows-ui/src/remote-audio.ts:42,115-126` 每包建立 Float32Array、AudioBuffer、AudioBufferSourceNode 并复制；480 样本 / 48kHz 对应约每秒 100 个包时，节点创建也达到同等频率。`14-16` 已有 40ms 提前量和 250ms 调度上限，不能称为无界队列。
- **方案：** 有界环形缓冲、复用内存、抖动/欠载指标和可调目标缓冲；按真实 AudioContext 状态反馈静音、恢复与播放失败。[AudioWorklet](https://developer.mozilla.org/en-US/docs/Web/API/AudioWorklet)可在独立音频线程处理数据。
- **验收：** UI 主线程忙时仍保持音频连续且不过期堆积；比较分配率、长任务、欠载及实测音频延迟，保留明确缓冲上限，不预先承诺改善百分比。

### DL2-018 · P1 · 为 Apple 编码帧输入增加有界解码调度

- [ ] 将逐帧转换/解码提交移出主线程，并约束解码前的积压。
- **证据 / 类型：性能结构。** `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:569-589` 每个 FFI 事件复制 Data 并 DispatchQueue.main.async，`442-450` 在主线程提交解码；`apps/apple/Sources/DeskLinkAppleCore/H264AnnexB.swift:26-45,80-81` 多次数组/Data 构建，`apps/apple/Sources/DeskLinkAppleCore/H264Decoder.swift:214-220` 再复制到 CMBlockBuffer。
- **现有保护：** `apps/apple/Sources/DeskLinkAppleCore/H264Decoder.swift:17-57` 已合并解码后的输出；本项针对它之前的队列和转换，不能重复报告输出无限缓存。
- **方案：** 专用串行解码队列、按字节/数量/年龄限制输入；保持配置、关键帧和 delta 依赖，必要时请求关键帧；NAL 用 range/slice 一次构建，UI 只发布最新 pixelBuffer 和低频指标。
- **验收：** 主线程阻塞后输入队列和 frame age 有上限；记录分配/copy bytes、解码/渲染时间、CPU/GPU/内存；不能任意丢 H.264 delta 破坏参考帧链。

### DL2-019 · P1 · 中继媒体拥塞不能阻塞连接控制与音频

- [ ] 分离读取与发送调度，为各类数据设置独立预算和时效。
- **证据 / 类型：性能结构。** `server/relay/src/lib.rs:92,1112-1135` 共用 256 个 datagram 转发槽，耗尽时在连接 select 分支内 await permit，暂停同循环 accept_bi/closed；`1392-1415` 视频、音频、光标共用路径，每包建任务/Vec，单包等待上限 250ms。
- **现有保护：** `server/relay/src/lib.rs:1080-1109` 已把建立的可靠通道放入独立 forward_reliable 任务，它们仍能继续转发；直接受阻的是新可靠流的接受、同循环关闭处理和后续 datagram 读取，不能据此断言所有现有键鼠/控制都被槽位直接阻塞。
- **方案：** 按连接常驻 sender 与有界队列，按音频、光标、视频的时效调度；视频以帧/依赖为单位丢弃过期数据；可靠控制/输入有独立预算，单个慢接收端不占满其他会话资源。
- **验收：** 慢接收端、关键帧突发、多会话压测下记录输入/音频 p95/p99、任务数、队列年龄、CPU/吞吐；关闭/连接事件不被媒体槽位等待卡住。实际容量需测试。

### DL2-020 · P2 · 关键帧可靠批次一次计长、一次编码

- [ ] 移除组批时反复克隆并序列化整个候选批次。
- **证据 / 类型：性能结构。** `apps/windows/src/runtime.rs:2263-2293` 每增加分片先 clone 当前批次/datagram，再 encode 判断大小；`2302` 发送时再次编码。`crates/desklink-protocol/src/lib.rs:66` 批次上限 60 KiB。
- **方案：** 依据 wire header 与分片长度增量计算边界，最终一次编码；复用 Bytes/切片，保留协议长度验证。
- **验收：** 最大合法关键帧的分配/复制次数下降；精确边界、跨批、超限和 round-trip 不变。此重复工作有单批大小上限，不是整个会话无限增长。

### DL2-021 · P1 · 慢磁盘写入与远控 async 调度隔离

- [ ] 为文件读写提供带字节预算、取消和完成语义的专用 worker。
- **证据 / 类型：性能结构。** `apps/windows/src/runtime.rs:3035-3044` 直接调用 `apps/windows/src/transfer.rs:360-373` 的同步 write_all 与 hash。初始化及完成已有 spawn_blocking，不能写成全部 I/O 都未隔离。
- **方案：** 长驻磁盘 worker 或有界异步管线，网络回压跟随落盘速度；不要每个 48 KiB chunk 新建 blocking task。两个传输方向一起核对；校验/flush 成功后才发布完成。
- **验收：** 慢盘、云盘、杀毒扫描、空间耗尽、取消同时发生时，媒体/输入仍满足预算、内存有界、残留可清理；不能显示尚未落盘的虚假完成。

### DL2-022 · P2 · 从 GDI 兜底恢复到高性能捕获后端

- [ ] 增加有退避、有限失败预算的 DXGI 恢复探测。
- **证据 / 类型：能力缺口、性能结构。** `apps/windows/src/capture.rs:551-565` 在运行时 DXGI 故障后永久替换成 GDI；`483-485` GDI 等待后完整截图，本会话没有回到 DXGI 的路径。
- **方案：** 保留现有可靠兜底，后台探测设备/桌面恢复，再按代数切回；显示实际后端和降级原因，防止来回振荡。
- **验收：** 睡眠、RDP/桌面切换、显示器恢复后，DXGI 可在不结束会话的情况下恢复；比较恢复前后资源与帧龄。不得为追求 DXGI 反复打断稳定 GDI。

### DL2-023 · P2 · 分离清晰度、帧率、码率与内容模式

- [ ] 重做能力协商及自适应策略，在能力允许时支持 4K/60fps 等配置。
- **证据 / 类型：能力限制。** `apps/windows/src/runtime.rs:106-123` 主要为 720p30/1.5Mbps、1080p30/2.5Mbps 和 Sharp；`apps/windows/src/encoder.rs:4-5,14,25-47` 常规 Sharp 上限 1440p30/18Mbps，4K 仍为显式实验；`apps/windows/src/runtime.rs:163-212` 主要依据丢包、decode queue 和 freshness 调档。
- **用户影响：** 档位跳转同时改变多个维度，文字清晰度与运动流畅度无法分别选择；高分屏/高刷能力未成为完整产品配置。
- **方案：** 连续码率与受约束的分辨率/帧率协商，加入编码耗时、排队年龄、吞吐和渲染压力；静态文字、滚动、视频模式分别决策并有迟滞。先完成 DL2-014～016。
- **验收：** 带宽阶跃、丢包、弱 CPU/GPU、静态文字及视频播放对照清晰度、帧龄、切档次数和恢复速度；不能把 4K60 对所有设备一刀切开启。

### DL2-024 · P2 · 将低延迟直连从视频优化扩展为完整数据路径

- [ ] 设计认证后的多通道 LAN 数据面，再按测量决定公网 NAT 穿透方案。
- **证据 / 类型：架构能力限制。** `crates/desklink-transport/src/video_egress.rs:5-8` 明确只优化视频；`apps/windows/src/runtime.rs:2424-2428` 音频仍通过 relay QuicClient；可靠通道位于 `crates/desklink-transport/src/quic.rs:925-930`。因此同网视频直连不代表键鼠/文件也在 LAN 内传输。
- **方案：** 同一已认证路径承载独立优先级的输入、控制、音频和文件，并支持逐通道切换、回退和去重；完善局域网发现/本地配对与 relay 不可达时的行为。公网打洞作为独立实验，不把“换 WebRTC”本身当性能结论。
- **验收：** 同网输入延迟符合实际 LAN 路径；断开公网后按定义保持本地连接；切换不重放按键、不串会话、不中断授权。公网方案另测不同 NAT/IPv6/UDP 限制与回退时间。

## 5. 功能完备与用户流程

### DL2-025 · P1 · 统一 Rust、C 头文件和 Swift 的能力契约

- [ ] 补全 ABI 命令/事件，并自动检查两份 C 头文件与实现一致。
- **证据 / 类型：能力缺口、契约缺陷。** `crates/desklink-ffi/src/controller.rs:330-379` 已有显示器、音频、画质、transfer 方法；`crates/desklink-ffi/src/worker.rs:70-75` 只有 Input/ReleaseAll/Keyframe/Shutdown；`crates/desklink-ffi/include/desklink.h:46-57` 事件止于 ReleaseAll，而 `crates/desklink-ffi/src/lib.rs:115-116` 已有 Transfer=11、Audio=12。Apple 还维护 `apps/apple/Sources/DeskLinkC/include/desklink.h`。
- **方案：** 为所有命令、事件、结果、错误及 capability 建立唯一契约来源，生成/校验 C 与 Swift 声明；事件携带代数、版本、可恢复性。通过有界 worker 命令暴露现有 runtime 能力。
- **验收：** 每个公开枚举、struct 布局、长度/所有权与命令均有 ABI 测试；新增事件不能无声落入 Swift default。此项是 DL2-026、027 和多个输入修复的共同前置。

### DL2-026 · P1 · 补齐 macOS → Windows 的会话功能闭环

- [ ] 将已有底层功能真正接到 AppleCore 与 macOS 会话界面。
- **证据 / 类型：能力缺口。** `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:336-350` 只消费 state/error/video/cursor，Control/Audio/Transfer 落入 default；`apps/macos/Sources/DeskLinkApp/Views/SessionView.swift:56-84` 缺少相应会话操作。Rust 有方法不能代表 Mac 用户已可使用。
- **分项交付：** 多屏列表/切换；会话内适应/1:1/全屏操作与清晰度；音频播放/静音/音量及失败恢复；纯文本剪贴板双向操作；文件双向选择、确认、进度、取消、重试与断点恢复。macOS 原生窗口全屏已有系统入口，本项不把它说成完全无法全屏。
- **方案：** capability 驱动操作可用性；每项提供等待、成功、拒绝、失败和下一步，不支持时准确说明；布局沿用平台原生习惯，控件命名与 Windows 保持任务语义一致。
- **验收：** Mac → Windows 每个分项都完成真实用户动作和错误恢复；接收文件仍经被控端授权，换设备不能恢复到错误目标；与 DL2-025、029～032 联合验证。

### DL2-027 · P1 · 在 Apple 会话呈现远端独立光标

- [ ] 解码并绘制形状、热点、位置和可见性，不依赖视频中烘焙光标。
- **证据 / 类型：能力缺口。** `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:346-348` 已写 cursorOverlay；`apps/apple/Sources/DeskLinkAppleCore/DeskLinkEvents.swift:202-209` 保存 encodedUpdate；macOS `apps/macos/Sources/DeskLinkApp/Views/SessionView.swift:18-22` 只有视频和输入层，Apple 展示路径没有消费此 overlay。
- **方案：** 独立低延迟光标层、shape cache、统一视频几何与 DPI 映射；处理隐藏、切屏和缩放后的热点。
- **验收：** 文本 I-beam、resize、忙碌、隐藏光标与四角热点在不同 DPI/缩放模式正确；仅光标变化不强制整帧重绘。

### DL2-028 · P1 · 重做 Apple 保存设备模型与长期重连

- [ ] 将设备身份、用户名称、授权类型和可更新的连接材料分离。
- **证据 / 类型：能力缺口。** `apps/apple/Sources/DeskLinkAppleCore/SavedHostStore.swift:4-9` 仅保存 UUID/serverName/sessionID 等；`apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift:96-103` 用同一 relay serverName 作为多个设备的主标签。`apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:210-218,467-477` 回放旧材料；`crates/desklink-ffi/src/lib.rs:881-889` 已保存材料的 secure 重连分支把 expiry/directory 置空。当前 directory 连接已有重新 lookup 与过期处理（`crates/desklink-ffi/src/worker.rs:694-734`），缺口是保存模型与后续回放未保留这些语义。
- **用户影响：** 多台电脑难区分；保存到 Keychain 不等于主机重启和 session 重建后仍可长期重连。
- **方案：** 稳定设备 ID、公钥、可编辑别名、授权类型/过期、上次连接、连接材料分别建模；固定访问重新解析目录并核对身份；临时授权失效明确要求重新连接。
- **验收：** 同一 relay 的多台电脑清楚可辨；固定主机重启/session 重建可恢复，临时密码刷新/过期与撤销不得盲目循环；设备身份改变需重新确认。

### DL2-029 · P1 · macOS 连接和等待批准期间提供取消

- [ ] 统一连接进度页，所有非终态等待均可取消或更换目标。
- **证据 / 类型：能力缺口。** `apps/macos/Sources/DeskLinkApp/Views/ConnectView.swift:63-71` 及 `apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift:108-119` busy 时只禁用连接；`apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift:118-121` connecting/pairing 尚未进入有断开按钮的会话页；`crates/desklink-ffi/src/controller.rs:44-47` 握手可等待 150 秒，等待批准/能力协商可等待 120 秒。
- **方案：** 展示查找、建立连接、等待批准、恢复等状态，始终提供取消；保留可重试的用户输入；generation 隔离迟到批准、错误及视频。
- **验收：** 每个阶段立即取消，再连另一台设备，旧结果不复活；取消本身满足 DL2-004 的时间和主线程响应预算。

### DL2-030 · P2 · 支持大文件与目录的完整传输体验

- [ ] 升级单文件边界与目录清单，完善大数据量续传、校验和进度。
- **证据 / 类型：能力限制、性能结构。** `crates/desklink-protocol/src/lib.rs:40-42` 单文件上限 **256 MiB**，`apps/windows/src/transfer.rs:35-41` 发送准备即拒绝；当前以单文件 offer/chunk 表达。`apps/windows/src/transfer.rs:90-119,265-307` 和 `apps/windows/src/runtime.rs:3172-3198` 恢复会重复扫描已有前缀进行 hash。
- **方案：** 保留协议已有的 u64 尺寸/偏移（`crates/desklink-protocol/src/lib.rs:463,473,481`），贯通大文件上限、UI/FFI 计数与空间预估；增加文件树 manifest、分块校验和可复用断点证明。重名冲突、符号链接/路径边界、部分成功、失败项重试与取消组成同一工作流，不能仅调大上限，把慢扫描转移给用户等待。
- **验收：** >4 GiB 文件、深层目录、中文/长名称、同名、稀疏文件和慢盘；断网/重启后仅在身份及源内容匹配时恢复；完整校验后才显示完成。保留现有双向传输和确认机制，不能误当它们未实现。

### DL2-031 · P2 · 让等待队列中所有文件都能管理

- [ ] 保留紧凑摘要，并允许展开完整队列、逐项移除。
- **证据 / 类型：能力缺口。** `apps/windows-ui/src/controller.ts:1131-1142` 固定 queued.slice(0,4)，只显示其余数量；最多 20 个等待项时，第 5～20 项没有逐项入口。
- **方案：** 完整列表、内部滚动、清楚的当前/等待/暂停状态，恢复队列使用同一界面。20 项无需为形式引入虚拟列表。
- **验收：** 加入 20 项后能直接定位并删除第 17 项，不影响其他项和正在发送项；键盘可完成同样操作。

### DL2-032 · P1 · 文件拖放只作用于明确的远控接收区域

- [ ] 对拖入提示与 drop 使用同一可见区域命中规则。
- **证据 / 类型：源码缺陷。** `apps/windows-ui/src/controller.ts:2147-2164` 在整个 WebView 监听 native drop，仅检查 runtime connected，没有核对当前视图及 drop position。
- **用户影响：** 后台仍有远控连接时，把文件拖入设置、共享或画布以外区域也会加入远端发送队列。
- **方案：** 仅当前可见远控 viewport 或明确文件面板接受；统一物理/逻辑坐标和 DPI，菜单/表单默认不接受；提交前核对目标设备及代数。
- **验收：** 100%/150%/200% DPI 与窗口缩放下，画布内生效，侧栏、设置、共享、菜单和窗口外不生效；离开连接页无残留拖入提示。

### DL2-033 · P2 · 明确宿主快捷键与远端快捷键路由

- [ ] 同时提供可发现的退出全屏、发送 Esc 和系统组合键操作。
- **证据 / 类型：能力限制。** `apps/windows-ui/src/controller.ts:334,3799-3806` 在 capture 阶段截获全屏 Escape 并 stopImmediatePropagation；`984-989` 没有发送远端 Esc 的替代入口。
- **方案：** 独立宿主键/组合键退出全屏，或明确“发送 Esc”菜单；组合键菜单覆盖远端常用系统操作，必要时允许配置；不要吞输入法取消键。
- **验收：** 全屏远程应用可关闭菜单/取消对话框，同时本地全屏也可可靠退出；中文组合、键盘布局、失焦及快捷键按下/松开一致。与 DL2-006、007 联合设计。

### DL2-034 · P2 · 账号表单错误后保留可纠正输入

- [ ] 为账号表单保存短期草稿，并使重发验证绑定明确邮箱。
- **证据 / 类型：源码缺陷。** `apps/windows-ui/src/main.ts:216-218` 没有草稿绑定；`317-325` 校验失败/提交 render 清空字段；`335-338` 注册后显示重发，但 `286-290` 再从已清空的 DOM 读取邮箱。
- **方案：** 邮箱在内存跨模式保留，pending verification email 独立于 DOM；字段错误就近定位；密码仅保存在短期内存并在成功或离开流程时清理。
- **验收：** 密码不一致、网络失败后邮箱仍在；注册成功可直接重发；不把密码写入 localStorage、日志或诊断。

### DL2-035 · P2 · 隔离账号迟到响应与用户后续选择

- [ ] 为账号动作增加操作类型、请求代数和视图所有权。
- **证据 / 类型：源码缺陷。** `apps/windows-ui/src/main.ts:211-223` 请求中仍能切换模式/进入本机模式，`327-349` 和 `295-302` 的完成结果未校验代数。迟到注册可切回登录，迟到登录可覆盖本机模式选择并再次初始化 workspace。
- **原生副作用：** `apps/windows-ui/src-tauri/src/account.rs:169-201` 在登录响应后保存会话，`apps/windows-ui/src-tauri/src/lib.rs:205-214` 随后重启 Host；只丢弃前端结果仍无法隔离这两步。
- **方案：** 请求捕获不可变的动作/邮箱/代数；切换使旧结果失效，必要时取消底层请求；原生持久化和重启 Host 前同样复核操作所有权。对已完成但用户已离开的登录明确处理本机会话，不直接使用会清空远控记录的 account_logout 作为补偿；不要求用户留在等待页，也不重复订阅 workspace。
- **验收：** 可控 Promise 覆盖注册中切找回、登录中进入本机模式、连续重试；旧结果不能覆盖新选择或重启后台共享。

## 6. 服务端与长期运行

### DL2-036 · P2 · 同邮箱并发注册保持幂等和稳定响应

- [ ] 把查重与创建改为数据库原子语义，正确处理唯一键竞争。
- **证据 / 类型：复现缺陷。** `server/account/src/services/account-service.ts:36-40` 先查询，await hashPassword 后插入；`server/account/src/db/store.ts:136-143` 普通 INSERT。两个并发同邮箱请求本次返回 202 与 500。
- **方案：** 原子 insert-on-conflict / 明确 unique conflict 结果；用户、设备及验证任务采用一致事务/幂等策略。重复请求返回相同的通用确认，不泄露账号存在性，也不重复轰炸邮件。
- **验收：** 同邮箱并发 N 次无 500、只创建一个用户，邮件/设备副作用有确定上限；不同邮箱并发不被全局大锁串行化。

### DL2-037 · P2 · readiness 反映真实依赖状态

- [ ] 将进程存活与数据库可服务分开检查。
- **证据 / 类型：复现缺陷。** `server/account/src/app.ts:46-56` 固定返回 ready；本次关闭注入的真实 AccountStore 后，`/ready` 仍为 200。
- **方案：** 低成本、带超时的数据库探测和 schema 状态；邮件故障单独暴露为可恢复降级，不让可选账号故障影响本机远控；输出稳定白名单状态。
- **验收：** 关闭/锁住/只读数据库以及未完成迁移时正确非就绪；健康路径低开销；health 不暴露凭据、路径或内部堆栈。

### DL2-038 · P2 · 邮件任务与账号历史记录具有完整生命周期

- [ ] 将邮件投递从用户请求等待中解耦，并补齐失败终态、回收与恢复。
- **证据 / 类型：能力缺口、性能结构。** `server/account/src/services/account-service.ts:158-178` 注册/找回直接 await 投递，`server/account/src/providers/http-mailer.ts:15` 可等待 10 秒；`server/account/src/db/store.ts:302-316` 在 attempts 达 10 后不再查询，但没有明确终态接口；schema 中 session/action token/outbox/audit 没有保留期回收入口。
- **现有保护：** 已有持久 outbox、进程内 single-flight、最多 10 次重试与退避，不能报告成没有重试或无限重试。
- **方案：** 请求原子入队后返回；独立 worker 认领/恢复/去重，暴露 pending/delivered/failed；按用途保留最小审计与刷新令牌重放检测窗口后分批回收；进程关闭时停止定时器并完成资源释放。
- **验收：** 邮件服务慢/宕机时注册响应不等待投递 timeout；第 10 次失败可诊断/受控重投；重启不丢任务；回收不破坏有效令牌和重放检测，长期数据库增长可解释。

### DL2-039 · P2 · 诊断保留期清理不扫描并锁住全部历史

- [ ] 为清理键补索引，并将大批回收移出每次接收事务。
- **证据 / 类型：复现性能结构。** `server/diagnostics/src/store.ts:17-47` 的事件索引只有 correlation/installation；`103-111` 每次 insert 事务内按 received_at_unix_s 删除 events/batches。本次真实 schema 的 EXPLAIN 返回 SCAN events。
- **方案：** 以 received_at 建索引，按时间/数量分批回收，独立低频维护；保留接收原子性和去重；监测 WAL、磁盘及清理耗时。SQLite 文件是否缩小需实际维护策略，删除行不等于空间立刻归还 OS。
- **验收：** 用真实规模合成历史记录验证 query plan、接收 p95、清理锁时间和磁盘上界；14 天保留语义不变，不为性能丢失批次原子性。

## 7. 代码整理与验证体系

### DL2-040 · P1 · 按实际所有权拆分巨型控制模块

- [ ] 将协议/会话策略、平台能力、媒体、输入、传输和 UI 生命周期分开，消除重复实现。
- **证据 / 类型：维护结构与已发生契约漂移。** 当前 `apps/windows-ui/src/controller.ts` 3854 行，`apps/windows-ui/src-tauri/src/controller.rs` 6028 行，`apps/windows/src/runtime.rs` 3869 行，`crates/desklink-ffi/src/lib.rs` 2169 行。行数本身不是 bug；DL2-001、003、007、025 已证明跨层策略/声明实际漂移。
- **方案：** Rust 共享 session/transport/policy 持有恢复、能力和背压规则；平台 adapter 只做捕获、注入、存储、权限；前端分为 session store、media presenter、input、transfer、shell。统一错误 code/状态/命令结果，减少依靠字符串和裸整数分支。
- **实施方式：** 随真实修复完成纵向切片迁移，删除被替代路径；允许直接调整内部 API/协议，但不把重命名目录或更换框架当作性能收益。用 CodeGraph 确认调用方，再以编译和测试验证。
- **验收：** 一项策略只有一个所有者，生产调用与测试调用相同；Windows 和 Apple 使用同一语义；每个新边界具有资源/取消/错误合同；通过 DL2-014 的前后性能比较。
- [x] **不冲突的公共合同整理（`docs(DL2-040): define boundary ownership and integration gates`）：** 新增 `docs/second-stage-boundary-contracts.md`，经 CodeGraph 查询/调用影响和本 worktree 源码逐项确认 SessionMachine、ReconnectSchedule、三处 transport 分类、两套 controller ConnectFailure、同步 join 和 ABI/Swift 映射。保留 persistent/临时邀请、host 服务/单次 controller 的语义差异，明确命令接纳/完成/终态、取消/资源/callback 所有权和稳定错误类别，记录已提交跨组接口与固定 SHA。
- **本切片验证：** 文档本地链接、10 处相关源码路径和 12 个完整提交身份可解析，独立只读复核无阻塞事实错误，`git diff --check` 通过；未修改 crates/apps/server 业务源码。既有 Windows 全栈验证保留原始源码身份，不冒称各组集成验证或新增性能结果。
- [ ] **仍依赖集成：** 009 协议13、Apple 003/004/025、网络024、媒体011/012/021与产品002等提交须在统一 revision 收口，再逐切片消除重复 owner；DL2-014 同硬件前后比较、Swift/Xcode、真实资源/取消/权限场景仍缺。当前只完成整理，主项继续未完成，不在本分支提前重写 controller/runtime/FFI。
- [x] **追加协调合同（`docs(DL2-040): record input epoch ownership and probe lane gates`）：** 明确 Apple/FFI 单一 owner 发布 `VideoConfig.input_epoch` / `InputEnvelope{stream_id,input_epoch}`，媒体 host 原子清键/推进/映射锁校验，产品实际首帧呈现后启用并在入队固定 context；旧输入/旧 release 必须隔离，取消与本机收回独立有效，不以 seq/wallclock/config_version 替代。013 严格绑定仍未完成。已核对媒体独立 probe lane 提交 `0836a8b311a858bf12f0d0b4427961c35f5986a7`；合同、调用方及接入验证须分别交付。文档链接、固定提交身份与 diff 检查通过，未改活跃模块。

### DL2-041 · P1 · 验证入口覆盖全栈与真实跨层边界

- [ ] 提供 Windows/macOS 可运行的统一验证命令，以及分层可选择的重型验收。
- **证据 / 类型：工程缺口。** `scripts/verify.sh` 覆盖 Rust、Windows 前端和部分 macOS，却未汇总 account、diagnostics、Python；原 TODO 的“FFI 已连接取消 / shutdown 背压”仍未关闭。246 项前端及 Rust 输入测试均通过，但 DL2-001 的不一致依然存在。
- **方案：** fast：format/typecheck/领域与契约测试；integration：本地 transport、慢发送/慢盘、取消/代数、跨 ABI；native：捕获/编码/权限/设备；performance：可重复基准与 soak。固定 Bun 1.3.14 与仓库 Rust toolchain，输出版本、源码 SHA、命令、跳过原因。
- **新增关键覆盖：** TS→Rust 滚轮边界；FFI→Swift 可恢复错误；AppKit 实际键盘/输入法接线；shell 更新不销毁会话；backpressure 下关停；静态关键帧；拖放坐标；拓扑变化；并发注册与依赖失效 readiness。
- **验收：** 从干净依赖环境可重复运行；缺少 Apple/设备条件时明确 skipped，不能算 pass；不以源码正则匹配代替关键用户流程。只为已稳定且有明确验收标准的界面增加 DOM/交互测试。
- [x] **统一入口代码与 Windows 自动验证切片（`feat(DL2-041): add layered local verification with source-bound reports`）：** `python scripts/verify.py` 统一 fast / integration / native / perf，PowerShell/POSIX 薄入口转发；固定 Bun 1.3.14、仓库 Rust toolchain 与 frozen/locked 安装，Windows Cargo 经原生环境 wrapper；任务可单选且自动带前置，记录源码 SHA/dirty、工具版本、命令、耗时和原始日志，零测试不能误通过，超时清理整个子进程树，清理失败阻断后续任务。说明见 `docs/local-verification.md`。
- [x] **入口回归：** Python 全套 127 项通过（新增入口回归 25 项，含真实子进程/子进程树、固定版本、零测试、skipped、失败阻断与间接 Cargo 依赖）；三个 Bun 项目的类型检查和测试通过（246 / 20 / 14 项），前端生产构建、Rust fmt/Clippy/workspace lib、protocol/video/session/crypto、localhost transport/relay、FFI runtime、媒体恢复和 Windows runtime 契约均通过。实际命令与结果另存 `dist/verify/verified-local.json`，运行时源码为基线 `a905b88` 加本切片，dirty=true；不是跨分支集成基线。
- [x] **跳过语义：** `python scripts/verify.py native perf --report dist/verify/verified-device-skips.json` 实际记录 Apple/交互设备缺条件与 DL2-014 未集成，`passed=false` / incomplete；固定错误 Bun 与零匹配测试分别验证为 failed。
- [ ] **仍需验收：** macOS/Xcode 实际运行、原生权限/硬件/双机/长稳与 DL2-014 同条件性能对照；离线媒体基线入口待媒体分支独立提交 `254f8be` 集成后执行。跨层新增关键覆盖仍随对应 DL2 修复补齐，主项保持未完成，不以本地入口成功代替用户流程验收。
- [x] **ABI 检查入口增量（`feat(DL2-041): register read-only ABI header verification`）：** 检查 Apple 已提交的 `5691aba` 后注册跨平台 `--task ffi-header-sync`，只调用生成器 `--check`，不自动改头；源码未集成时 skipped。新增两项合成回归验证未集成不执行、同步检查不改头、漂移失败；统一入口 Python 全套 134 项通过（入口回归合计 27 项），证据 `dist/verify/abi-gate-python.json`。真实本基线 `--task ffi-header-sync` 记录 incomplete / passed=false（`abi-gate-unintegrated.json`），实际 ABI layout / Swift 仍待集成后验证，未复制其他分支 WIP。

### DL2-042 · P2 · 统一当前能力、版本与阶段记录

- [x] 更新产品能力矩阵和当前架构契约，明确用户确认、源码能力与本次验证的边界。
- **证据 / 类型：文档缺陷。** `docs/current-architecture-contract.md:10,22,74` 仍以 0.1.91 为当前候选；package/README 已为 0.1.93。原 TODO 把 macOS VideoToolbox 当未来项，但 `apps/macos/Sources/DeskLinkApp/Capture/MacH264Encoder.swift` 已有实现；原“下一阶段”还停留在 Windows 首发冻结。
- **方案：** 以 Windows→Windows、macOS→Windows 为核心矩阵，逐项列输入/IME/音频/剪贴板/文件/多屏/权限/恢复；被控 Mac 和 iOS 单列。版本由 manifest/配置读取，历史发布验收与新阶段开发任务分离。
- **验收：** README、PRODUCT、DESIGN、架构契约及验证脚本的能力描述一致；用户确认的正常控制不再被标为未实现，尚未接通的 Mac 功能也不能因 Rust 方法存在而打勾；旧检查记录保留日期和原始范围。
- **体验约束：** 维持连接/共享两主入口、原生平台习惯和清楚的下一步；本次不把既有视觉取舍、没有装饰或旧阶段未签名一概判成远控功能缺陷。
- **完成（`docs(DL2-042): align current capabilities and manifest version evidence`）：** 新增 `docs/current-capabilities.md`，以 main `a905b88` / 工程切片 `44a2a99` 为基线，逐方向列基础控制、IME、音频、剪贴板、文件、多屏、权限和恢复；同步 README、PRODUCT、DESIGN、架构契约，保留历史记录日期。明确用户已确认两条基础远控、Mac VideoToolbox 已实现、未接通 Apple UI 不能因底层方法存在而勾选；其他分支只列固定提交证据，035 三笔链和 009 协议 13 契约不冒充已合入 main/完整反馈功能。
- **实际验证：** 只读 `python scripts/verify-windows-release.py --versions-only` 输出 manifest 的 0.1.93 / 协议 12 / Bun 1.3.14 / Rust 1.97.1；5 项新增元数据/版本漂移回归通过，空 PATH 的真实 CLI 不调用构建/原生准备，文档版本表与 README 动态对照 manifest。统一入口 `--task python-tests --report dist/verify/dl2-042-python.json` 全套 132 项通过；6 份相关文档的本地链接全部可解析，`git diff --check` 通过。源码验证为 `44a2a99` 加本切片（dirty=true）；本次未新增真机、发布或性能结论。

## 8. 扩展角色

### DL2-043 · P3 · 被控 Mac 的能力协商与实际编码一致

- [ ] 在扩展 Windows→Mac / Mac→Mac 前，统一声明能力和真正的捕获编码参数。
- **证据 / 类型：源码契约缺陷。** `crates/desklink-ffi/src/host_worker.rs:741-748` 声明 Main/1920×1080；`apps/macos/Sources/DeskLinkApp/Capture/MacH264Encoder.swift:355-362` 实际配置 High，`apps/macos/Sources/DeskLinkApp/Capture/ScreenCaptureSource.swift:87-105` 默认捕获上限为 2560×1440。
- **方案：** 根据实际 VideoToolbox 后端能力构建协商结果，确保每次重配的 profile、尺寸和配置版本一致；不向对端发送其没有声明支持的码流。
- **验收：** Main-only 控制端、High 支持端、编码器失败降级、切屏/分辨率变化分别验证；真实 Mac 系统权限、签名/公证和无人值守边界另记。此项不阻塞已经跑通的 Mac→Windows。
- **iOS 关系：** AppleCore 共享修复需要回归 iOS；iOS 新功能和独立发布不自动进入本阶段关键路径。

## 9. 建议执行顺序与依赖

1. **先固定合同和复现用例：** DL2-014、025、041 建立基线；并行修 001、003、034～037。补测试应直接针对上述可复现/可推导分支。
2. **消除操作与恢复断点：** 002、004～013、027～029、032。优先证明正常控制不会因菜单、重试、输入法、静态桌面或取消而失效。
3. **推进媒体性能：** 015/016 联合重构 Windows 捕获编码；018 优化 Apple 输入队列；017、019～022 分别优化音频、中继、文件和恢复。每项带同条件测量。
4. **补齐完整工作流：** 026、030/031、033、038/039；再以 023/024 推进高分辨率/高帧率和完整直连能力。
5. **同步收拢代码与文档：** 040 随纵向修复落地，042 随能力变化更新；043 作为扩展角色独立验收。

不要求按模块全部修完后才集成。每个纵向切片交付一条真实用户路径，避免出现“后端 API 已有，但 UI/ABI 未接通”的新半成品。

### 建议的性能验收目标

以下为第二阶段的初始工程目标，需要 DL2-014 在指定硬件/网络下校准，**不是当前实测值或对所有机器的承诺**。固定同画面、同画质、同路径后比较。

| 场景 | 初始目标 / 验收约束 | 测量方法 |
| --- | --- | --- |
| LAN 输入→画面反馈 | 参考 p95 ≤50ms；单独记录注入与视频各段 | 回显程序或外部高帧率录像；不能用 RTT 代替总延迟 |
| 1440p60 能力机 | p95 处理不积累超过一个帧周期；落后则降载 | 阶段单调计时与有界队列指标，记录实际显示而非仅编码 FPS |
| 静态画面恢复 | 关键帧请求→重新显示 ≤1 秒 | 主动重置解码器/丢关键帧后保持桌面静止 |
| 取消与退出 | 常规 UI 反馈 ≤100ms；完整停止建议 ≤2 秒 | 已连接、对端不读、发送背压和资源释放回归 |
| 不相关菜单/Host 更新 | 解码器重建次数 0，额外关键帧请求 0 | 会话存活期间记录资源创建/销毁与请求计数 |
| 音频 | 缓冲严格有界，UI 忙后不播放历史积压 | 欠载、排队年龄、实测音频延迟与断续记录 |
| 长稳 | 至少 8 小时，内存/句柄/线程/任务数无持续增长 | 固定工作负载，区分缓存平台期与持续泄漏趋势 |
| 慢盘/弱网/候选黑洞 | 输入、音频与取消不被慢路径秒级阻塞 | 故障注入并记录 p95/p99、锁持有与队列年龄 |

### 完成记录模板

每项任务在原条目追加：

- 修复提交 / 验证源码 SHA：
- 自动测试及命令：
- 真实设备、系统、硬件、网络与操作步骤：
- 前后性能数据与测量条件：
- 尚未验证、跳过项及原因：
- 最终状态：代码完成 / 自动验证完成 / 真机完成，分别勾选。

不得为了减少本清单数量，把未验证条目直接标成已完成。

## 10. 第一阶段历史记录

以下原文按本次审查前状态保留，仅供追溯。里面的“当前”“未验收”“下一阶段”、旧版本、线上探针结果和勾选项均属于原记录日期；第二阶段以本文前九节为准。尚未完成的签名、安装或设备组合验收可以继续引用，但不能用来否定用户本次已确认的两条远控路径。

<details>
<summary>展开第一阶段原始 TODO、发布门禁与历史证据</summary>

# DeskLink 上线 TODO（历史原文）

> 目标：先把 Windows 两台电脑的远程控制做成可发布的稳定版本，再扩展 4K 和公网 P2P。
> 当前判断（2026-08-15）：Windows 首发核心功能、自动化门禁、公网中继和云诊断均已通过；稳定候选必须以 clean `main` 以及同一批安装包 manifest、候选 ZIP、中继镜像和审计报告中的源码 SHA 为准。正式上线仍受 Authenticode 签名、发布 tag 和真实 Windows/SmartScreen 验收约束。账号服务已降为可选能力，未部署账号服务不再阻塞本机模式。
> 规则：`[x]` 只表示代码或自动验证已完成；需要两台真实 Windows 电脑的项目必须由人工验收后再勾选。

## 本地底座整理（2026-10-21）

- [x] 2026-10-23：0.1.92 接入启动检查、客户端更新提示、下载 / 取消 / 重试、独立签名校验和安装器更新交接；246 项前端、90 项桌面端、19 项安装器测试通过。实际公共版本发布和双机在线升级仍待验收，见 [客户端更新](docs/windows-client-updates.md)。
- [x] 2026-10-23：修复真实双机反馈的控制端中文输入法缺口，增加可编辑输入承载、组合提交 / 候选键处理和有序文本发送，保留现有协议；232 项前端、82 项桌面端测试通过，见 [中文输入修复记录](docs/2026-10-23-chinese-input-fix.md)。

- [x] 修复账号混合窗口限流、诊断批次原子写入、首次 join 超时与可靠流取消边界。
- [x] 修复恢复关键帧分片乱序时误清本帧数据，并以确定性分片顺序和跨帧确认屏障验证。
- [x] 修复主机审批刷新、审批焦点 / 过期禁用，以及进入账号页前的远程展示资源清理。
- [x] 移除 Windows 旧视觉主题，建立独立的布局、原生控件与交互 / 可访问样式基础。
- [x] 重新设计连接 / 共享双主入口与简洁青绿界面，统一表单、列表和设置；194 项前端测试与生产构建通过，真实双机验收仍独立进行。
- [x] 恢复已知 V1 配对记录的只读兼容，保留原 DPAPI 文件；修复共享状态订阅竞态，并恢复缺失的公网中继及独立证书续期。202 项前端测试、77 项 Tauri 测试、旧记录回归、原生连接 / 重连测试和公网探针通过，见 [连接与共享修复记录](docs/2026-10-21-connection-repair.md)。
- [x] 完成临时 / 固定密码与断线重连的本地流程复核，修复邀请重用的异常断开、密码页迟到响应与连接快照竞争；213 项前端、80 项 Tauri、8 项主机集成测试通过，重连后确认实际 H.264 出帧。真实双机操作验收仍独立进行。
- [ ] 补齐 FFI 已连接发送阶段的取消 / shutdown 背压回归与退出策略。
- [ ] 整理覆盖 Rust、三个 Bun 子项目和 Python 的本地验证入口。

本轮证据与后续拆分方向见 [底座检查记录](docs/2026-10-21-foundation-review.md)。以下线上、安装与真实设备记录保留原日期，本轮没有重新宣称其通过。

## 0. 发布基线与工作区（P1）

- [x] 整理当前工作区变更，确认 DirectLan、视频质量、UI、诊断和本机账号模式都属于本次发布范围。
- [x] 删除未发布版本仍保留的旧 host 安装入口和旧 relay join/directory 信封路径；当前构建只维护单一安装入口与当前协议线。已知 V1 本地配对记录于 2026-10-21 恢复只读兼容，不改变网络协议。
- [x] 将 Windows-only 发布边界接入 `verify-windows-release.py`，防止 README 重新宣传未纳入本版本的 macOS/mobile 产品面。
- [x] 让发布验证、安装包清单和 GitHub Release 绑定同一份源码提交 SHA，并拒绝脏工作区进入签名发布。
- [x] 让人工验收记录同时绑定候选安装器的精确文件名和 SHA-256，避免发布证据指向同哈希的非候选文件名。
- [x] Windows CI 和签名候选 artifact 自动上传与安装包绑定的人工验收模板，避免测试人员手工复制版本或 SHA。
- [x] 远程画面质量使用最近 10 秒滚动窗口，并在明显卡顿时提供一键切换流畅画质的恢复动作。
- [x] 防止连接命令快照覆盖实时状态通道，避免连接成功后界面短暂回退到旧状态。
- [x] 直连视频静默 5 秒后自动回落中继，并记录 `datagramSilence` 原因，避免直连黑屏卡死。
- [x] 将账号服务降为可选能力；未登录时主机仍启动，Windows 本机模式可直接使用，账号流程保留在“更多”菜单。
- [x] 更新版本号、变更日志和发布说明，形成当前候选基线（正式 tag 仍等待签名与人工验收）。
- [x] 在干净工作区上通过 Rust、Bun、安装包和发布校验（2026-08-13；本地候选门禁通过，Windows CI 历史基线 runs `30571054699` 与 `30572591597`）。
- [x] 将原生 Windows resilience 报告绑定源码 SHA 并纳入 readiness；捕获、编码、主机恢复、电源恢复和媒体 soak 证据不可再脱离候选物料。
- [x] 为桌面捕获增加首帧保障：DXGI 静态桌面超时后执行一次 GDI 初始快照，并在首帧 10 秒内未产生时交付可恢复的捕获错误，避免连接在线但画面永久黑屏。
- [x] 修复可靠视频配置与不可靠视频分片乱序导致的静态首帧丢失：控制端以有界队列暂存配置前分片，配置到达后回放并继续使用独立光标通道。
- [x] 为 Windows 视频管线增加首个编码帧看门狗：捕获已开始但编码器连续 10 秒不产出访问单元时交付可恢复的编码错误，避免 `NeedMoreInput` 导致假在线黑屏。
- [x] 停止/退出路径清理残留的配对状态，覆盖 worker 超时 abort 后的下一次启动回归测试。
- [x] 为普通主机 worker 的准备、停止和异常事件增加生命周期代数隔离，防止旧任务在重启后覆盖当前状态。
- [x] 将生命周期代数检查、状态发布和 worker 启动串入同一同步闸门，覆盖检查后写入和停止后启动的竞态。
- [x] 为控制端连接循环传递连接代数，并隔离迟到的旧连接状态事件，覆盖取消、切换设备和重连中的回写。
- [x] 为设备查询阶段的中继连接和目录请求增加连接代数取消闸门与 8 秒超时，避免查找设备时网络黑洞导致界面永久等待。
- [x] 为已保存设备连接阶段的中继建连增加同一连接代数取消闸门与 8 秒超时，避免跳过目录查询时重新进入无限“建立连接”。
- [x] 为 Windows 主机首次连接和重连增加 8 秒中继连接上限，网络黑洞会进入既有退避恢复，而不是长期停在“连接中”。
- [x] 为主机与控制端的 relay join 握手增加同一 8 秒超时，避免 TCP/QUIC 已建立但中继不返回 join 确认时继续假连接。
- [x] 为原生 FFI 控制端、主机端的建连、重连与目录查询补齐同等有界超时，避免非 Windows 宿主调用长期阻塞连接 worker。
- [x] 为中继同一主机身份重复启动补充替换回归测试；旧主机连接迟到断开时不会清除新主机、控制端或设备目录登记。
- [x] 传输层对带代数的陈旧 `PeerDisconnected` 增加 250 毫秒替换确认窗口；新对端首个可靠流到达时丢弃旧断开，真实断开仍在窗口后交付，避免控制端无谓重连。
- [x] 将持久连接标记写入新的 DPAPI 控制端存储格式；固定密码主机的目录暂时不可见或 relay session 重建时进入可取消的 6 段恢复窗口，临时邀请仍在失效后立即停止。
- [x] 固定密码连接在首轮退避预算耗尽后继续以最大间隔等待主机恢复；只有用户取消、认证不匹配或临时邀请失效才会停止，避免已验证的连接退回手工重试。
- [x] 将“连接设备”保留为唯一一级导航，其他管理入口统一收进“更多”菜单，并用 v-sans / 系统字体契约固定 1920/2560 宽屏层级。
- [x] 首次启动默认进入本机连接工作区，账号服务保持可选且不阻塞远程控制。
- [x] 本机模式快照读取失败时启用同等的自动恢复计划，避免首次启动停在暂时性错误页面。
- [x] 为可选账号服务请求增加 12 秒客户端超时，账号网络异常不会阻塞本机远程控制或设置页面。
- [x] 账号存储不可用时以未登录本机模式启动，不再因可选账号模块初始化失败退出整个应用。
- [x] 账号 HTTP 客户端初始化异常时保持账号能力快速失败，不回退到无界网络请求；本机模式仍可继续使用。
- [x] 正式 Release 上传前重新验证最终安装器 Authenticode 签名，防止签名清单与实际二进制脱钩。
- [x] 中继证书加载改用 `rustls::pki_types::pem::PemObject`，移除已停止维护的 `rustls-pemfile` 运行时依赖；workspace 级 RustSec 告警仍按平台依赖单独复核。
- [x] 将 RustSec 标记为线程安全问题的 `event-listener` 从 5.4.1 升级到已修复的 5.4.2；剩余 GTK/GLib 告警仅来自非 Windows 桌面依赖链，继续按平台边界跟踪。
- [x] 发布预检同时校验公网中继探针、线上容器源码 revision 与候选提交，避免旧中继通过网络探针但与候选版本不一致。
- [x] 短时公网探针显式关闭所有临时 QUIC 连接，避免监控探针把中继主机登记和会话容量占用到空闲超时。
- [x] 候选 Windows bundle 按当前提交和安装包哈希打包，防止同版本 ZIP 混入旧构建物料。
- [x] 发布预检同时校验 annotated tag 的解引用提交必须等于候选 HEAD，避免旧提交上的同名 tag 错误放行。
- [x] 人工验收记录升级为结构化 schema 2；双机、安装信任和 4 小时长稳通过时必须分别提供脱敏环境、安装和指标证据，不能只填写布尔值。
- [x] schema 2 验收记录启用字段白名单与敏感文本拒绝，阻止设备 ID、密码/密钥标签、PEM 私钥和本地 Windows 路径进入候选 ZIP。
- [x] 参考 RustDesk、MeshCentral、FreeRDP 的公开工程实践，整理当前产品不采纳项与后续 relay 高可用、安全扫描和会话诊断方向，见 [GitHub 同类项目架构对照](docs/github-architecture-benchmark.md)。
- [x] 修复静态桌面导致编码 resilience smoke 只收到首帧的问题；测试复用最近捕获帧并推进单调时间戳，运行时避免按固定帧号重复触发关键帧请求。
- [x] 接入独立安全扫描门禁：CodeQL 覆盖 Rust/TypeScript，Cargo audit 阻塞真实漏洞和 yanked 包；非 Windows Tauri 图形依赖的历史维护告警保留在日志中单独跟踪，见 [安全扫描门禁](docs/security-scanning.md)。
- [x] 为控制端连续滚轮输入增加有界合并，避免 IPC/网络阻塞时输入队列无界增长；键盘和鼠标按键仍保持严格顺序。
- [x] 将被控端单个被拒绝、过期或越界的远程输入样本隔离处理；缩放、切屏或迟到鼠标事件不会再重建主机 worker。
- [x] 将控制端和主机端的文件发送后台完成通知都改为容量为 1 的有界通道，断线期间迟到任务不会在旧会话中积累无界通知。
- [x] 将被控端捕获线程的屏幕/画质/profile 命令队列改为容量 4 的非阻塞有界通道，捕获线程卡住时沿既有恢复路径退出，不让控制循环无限积压命令。
- [x] 将 Windows 发布验证加入生产 Rust 无界 channel 静态门禁，防止后续稳定性修复重新引入不可控的事件积压。
- [x] 增加独立 Windows nightly/手动 300 秒 resilience 工作流；普通 PR 保持短反馈，长期捕获、编码、恢复和中继回归持续运行并上传脱敏证据。
- [x] 为 relay 目录查询限流表增加固定容量和最旧项淘汰，防止伪造设备 ID 的查询在单个限流窗口内消耗无界内存。
- [x] 为 relay 每条已认证连接限制可靠流转发任务为 16 个，异常连接超限时关闭，避免 Tokio 任务数量无界增长。
- [x] 为 relay 连接 admission 拒绝路径增加 64 个任务的上限；超出上限的 QUIC 握手立即拒绝，避免连接洪泛在达到容量后继续派生无界任务。
- [x] 为 Windows 主机 DirectLan 候选探测增加 2 个并发任务上限；重复候选超限时明确拒绝并继续使用中继视频，避免认证会话内的探测任务无界增长。
- [x] 为 Windows 托盘状态命令使用容量 128 的非阻塞有界队列；托盘窗口忙碌或队列暂满时丢弃提示，不阻塞主机连接、视频和恢复线程。
- [ ] 创建 `v0.1.91`（或下一版本）发布 tag，并保留可回滚提交。

## 1. 云端诊断可用性（P1）

- [x] 将 `desklink-diagnostics` 的 `/desklink-diagnostics/health` 和 `/desklink-diagnostics/v1/batches` 路由加载到生产 Nginx。
- [x] reload Nginx 后验证公网 health、限流和批量上报接口。
- [x] 运行 `scripts/audit-managed-diagnostics.py`，保存审计报告。
- [x] 在 Windows 客户端完成一次脱敏诊断上传，确认云端可检索且不包含密码、密钥、屏幕内容。

## 2. 局域网直连闭环（P1）

- [x] 让 `ControllerRuntime` 的 DirectVideoPath 状态机真正驱动候选探测和激活，而不是只根据已建立连接推断路径。
- [x] 增加认证探测、状态机激活和视频 datagram 传递的 transport loopback 测试；探测本身不会放行 4K。
- [x] 覆盖控制端对端先发起探测时的并发窗口，避免 Noise 会话锁导致假超时。
- [x] 被控端增加入站 DirectLan 探测监听，与已有出站连接互为兜底；Rust Windows 包测试、Clippy 和认证 datagram 回环回归测试已通过。
- [x] 补充 DirectLan transport 回环 E2E 夹具：成功、状态机超时、候选过期、认证绑定失败、主动关闭；HostRuntime ↔ ControllerRuntime 双机仍由 Windows 验收。
- [x] 视频 datagram 发送失败时立即撤销 DirectLan 句柄并重发同一包到中继；自动回落测试通过。
- [x] 增加 FFI 回归测试：直连候选被拒绝后视频回落中继、回落原因可观测，控制输入和关键帧请求仍保持中继；剪贴板/文件的双机行为继续由人工验收。
- [x] 增加直连 RTT、丢包和当前路径诊断字段；回落后仍保留最近探测质量和回落原因，避免用户看到“已直连”但实际仍走中继。

## 3. Windows 双机验收（P1，需人工）

- [ ] 同一局域网：两台 Windows 配对、审批、视频、鼠标、键盘、多屏切换。
- [ ] 不同网络：通过公网中继完成同样流程。
- [ ] 断网/切 Wi-Fi/中继重启后自动恢复，不出现重复会话或“主机服务已停止”。
- [ ] 睡眠唤醒、锁屏、窗口最小化/恢复、DPI 不同和双屏场景。
- [ ] 剪贴板、单文件/多文件传输、暂停/恢复、取消和失败重试。
- [ ] 连续运行至少 4 小时，记录 CPU、GPU、内存、帧率、延迟和错误日志。

## 4. 安装与信任（P1）

- [ ] 获取 Windows Authenticode 代码签名证书并安全保存 PFX。
- [ ] 配置 GitHub Actions secrets：`WINDOWS_SIGNING_PFX_BASE64`、`WINDOWS_SIGNING_PFX_PASSWORD`。
- [ ] 运行签名构建，确认安装包和主程序签名有效、证书链完整。
- [ ] 在全新 Windows 账户验证 SmartScreen、WebView2 Runtime 检查、安装、升级、卸载和数据保留策略。
- [ ] 生成带 SHA-256、签名状态和构建提交的发布清单。

## 5. 产品文档与 UI 收口（P2）

- [x] 清理 README 和历史版本说明，移除“直连尚未启用”等过时描述。
- [x] 清理安装器和 relay wire 的旧兼容分支；本地配对仅额外接受严格校验的已知 V1 格式，保存仍使用 V2。
- [x] 明确产品能力边界：当前最高 2560×1440；4K 和公网 NAT 穿透暂不承诺。
- [x] 统一 `DESIGN.md` 与 Windows UI 样式 token，删除多轮迭代产生的冲突规则。
- [ ] 手动验收键盘导航、焦点可见性、高对比度、WebView2 缩放和中文文案溢出。
- [x] 将发布前检查、服务器回滚和日志脱敏流程写入 [Windows 发布运行手册](docs/windows-release-runbook.md)。
- [x] 将同网/跨网、断线恢复、双屏/DPI、剪贴板/文件和长稳验收步骤整理到 [Windows 双机验收记录](docs/windows-two-pc-acceptance.md)。

## 6. Apple 端开发与验收（2026-07-26）

- [x] Rust FFI 增加显式 macOS/iOS controller platform，保留 macOS 默认 ABI 入口。
- [x] 抽取跨 macOS/iOS 的 `DeskLinkAppleCore`，统一 Keychain、controller bridge、H.264 解码和视频几何。
- [x] 完成 macOS Apple Silicon controller/host bundle、权限声明、验收脚本和未签名检查。
- [x] 完成 iOS 16+ controller-only 工程、device/simulator Rust xcframework、配对和保存设备。
- [x] 完成 iOS Metal 视频、直接触控/轨迹板、键盘、快捷键栏和 scene lifecycle 恢复。
- [x] 通过 Rust/Swift package/iOS device build/simulator unit+UI 自动门禁。
- [ ] 在真实 iPhone 上安装签名包，验收 Keychain、二维码、视频、触控、键盘、断线和前后台恢复。
- [ ] 完成 macOS Screen Recording/Accessibility 权限与同机双实例 controller/host 验收。
- [ ] 完成 Windows↔macOS、Windows↔iOS、macOS↔iOS 跨设备验收，以及 macOS 签名/公证。

详细记录见 [Apple 平台验收汇总](docs/apple/2026-07-26-apple-platform-acceptance.md) 和 [iOS controller 开发说明](docs/apple/ios-controller-development.md)。

## 7. 后续增强（不阻塞首个 Windows 正式版）

- [ ] 4K 原生编码能力评估、硬件矩阵和自适应码率策略。
- [ ] 公网 STUN/ICE/TURN 路径评估，决定是否增加真正的跨网 P2P 视频通道。
- [ ] 增加第二 relay 节点、故障切换演练和容量/证书续期告警；保持业务协议与端到端密文不变。
- [x] 增加独立 CodeQL 与依赖审计门禁，区分新增安全问题和历史平台依赖告警。
- [ ] UAC/安全桌面、语音输入、虚拟桌面等是否纳入下一版本范围。
- [ ] macOS VideoToolbox 适配（仅当恢复跨平台发布目标时）。

> 4K 评估已补充真实 Windows 捕获/编码探针和放行门槛，待真实设备运行并记录硬件矩阵后再勾选完成；见 [Windows 4K 编码评估](docs/windows-4k-evaluation.md)。

## 下一阶段执行顺序（发布冻结前）

1. **候选基线**：以当前提交和安装包 SHA 生成候选验收记录模板，确认发布说明只描述已验证能力，不扩展 4K 承诺。
2. **双机验收包**：使用现有 `verify-windows-resilience.py --soak-seconds 300` 和候选安装包，记录同网直连、跨网中继、断线恢复、双屏/DPI、剪贴板和文件传输结果；正式发布预检会拒绝低于 300 秒的 resilience 报告。
3. **信任与安装**：用户提供 Authenticode 证书后执行签名构建、SmartScreen 和全新账户安装升级验收；没有证书时保持候选版，不伪装成正式发布。
4. **发布冻结**：在干净 checkout 重跑所有门禁，生成 SHA-256/签名清单，最后创建 `v0.1.91` tag；4K、公网 P2P 和 macOS 继续留在后续版本。

## 自动门禁

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cd apps/windows-ui
bun test
bun run build
cd ../..
python scripts/verify-windows-release.py
python scripts/build-windows-installer.py
python scripts/create-windows-acceptance-record.py --operator "待真实 Windows 验收"
python scripts/verify-windows-resilience.py --soak-seconds 10
python scripts/verify-managed-relay.py
python scripts/audit-managed-diagnostics.py
python -m unittest discover -s scripts/tests -p "test_*.py"
python scripts/check-windows-release-ready.py --manual-json dist/windows/windows-acceptance-record.json
python scripts/package-windows-candidate.py
```

### 最近一次自动门禁记录（2026-08-15）

- [x] 当前候选 manifest 已通过前端 177 项测试、Vite 构建、Rust fmt/Clippy/workspace 测试、Python 75 项脚本测试、Windows release verification 和安装包构建；连接快照、主机生命周期竞态、字体契约、强制配色、1920/2560 宽屏、“更多”菜单键盘导航和无效远程输入隔离回归测试已纳入门禁。
- [x] Windows CI run `31840782145` 全部通过：GDI/显示器捕获、H.264 High Profile/4K 探针、主机异常恢复、电源恢复、本地媒体 soak，以及公网中继目录/Noise/视频/输入/重连 E2E；候选 artifact 的精确源码 SHA 以同批 manifest 和 readiness 报告为准。

- [x] 安全扫描 run `31840782167`：Cargo advisory audit、Rust CodeQL、JavaScript/TypeScript CodeQL 全部通过。
- [x] Backend services CI run `31755376304` 通过；Managed relay monitor run `31755577111` 与 Managed diagnostics monitor run `31755578886` 均通过。
- [x] 签名候选 readiness 明确传入 `--minimum-soak-seconds 300`，不会因为默认值变化而把正式长稳门禁降级为普通 CI 的 10 秒。
- [x] 签名候选工作流已强制 300 秒原生 resilience soak，并由 `test_windows_workflow_policy.py` 防止回退到普通 CI 的 10 秒快速门禁。
- [x] 候选物料已由 Windows CI manifest 完成来源绑定；正式验收或发布时必须直接读取对应 run 的 artifact manifest，不把动态 SHA 回写到源码，避免证据提交改变被验证的 source commit。
- [x] 发布脚本与签名工作流现在都会严格校验 readiness 报告，并把该报告作为 Release 证据上传；当前候选 `ready: false`，不会被误发布为正式版本。
- [x] GitHub Actions 已统一到 `actions/checkout@v7`、`actions/upload-artifact@v7` 和 `actions/download-artifact@v8`，旧的 Node.js 20 deprecation warning 已不再作为当前候选的工作流风险。

### 先前自动门禁（2026-07-28）

- [x] Rust workspace：`cargo fmt --all -- --check`、`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`。
- [x] Windows 发布验证：`python scripts/verify-windows-release.py`（155 个前端测试通过，TypeScript/Vite/Rust release 构建通过）。
- [x] Windows 安装包构建：`python scripts/build-windows-installer.py`（安装包清单生成，当前 `signed: false`）。
- [x] 主机 DirectLan 接入回环：`cargo test -p desklink-windows --lib runtime::direct_video_tests::host_acceptor_keeps_authenticated_direct_datagram_connection`。
- [x] DirectLan 回环边界夹具：成功传输、候选过期、认证绑定失败和主动关闭；`cargo test -p desklink-transport direct_probe::tests`。
- [x] 直连质量可观测性：记录当前路径、最近 RTT、丢包基点和回落原因；`cargo clippy --workspace --all-targets -- -D warnings` 与 Windows UI 控制器测试通过。
- [x] 中继探测：`python scripts/verify-managed-relay.py`（双向控制探测通过；实际耗时以同批 `managed-relay-verification.json` 为准）。
- [x] 云诊断审计：`python scripts/audit-managed-diagnostics.py`（公网 health、服务、定时器和报告新鲜度通过）。
- [x] 变更检查：`git diff --check`。
- [x] 增加统一 Windows 发布预检：`check-windows-release-ready.py` 汇总来源 SHA、安装包完整性、签名、tag、中继/诊断证据，并把真实双机验收明确列为人工阻塞项。
- [x] 增加候选绑定的人工验收记录模板：版本、来源 SHA、安装包 SHA-256、操作者和 UTC 时间必须与预检候选一致。
- [x] Windows CI 与签名工作流上传统一发布预检报告，保留来源 SHA、安装包完整性和未完成阻塞项的构建证据。
- [x] 账号服务 CI 纳入 `server/account` 测试与类型检查；账号服务仍可不部署，Windows 本机模式不依赖它。

## 当前已知事实

- 中继实况探测已通过：`101.35.246.159:4433`。
- 本地诊断服务、定时器、公网诊断 health 和 Windows 脱敏 HTTPS 上报已通过审计；服务器诊断当前发布为 `8d92cdaee6d3`，最近一次 Nginx 配置备份在 `/etc/nginx/conf.d/p2p.yxswy.com.conf.bak-desklink-1784743477`。
- 当前候选物料以本地 `dist/windows/windows-installer-manifest.json`、`windows-release-verification.json` 和 `windows-release-readiness.json` 为准；安装器仍未签名，正式验收时必须读取同批清单中的 source commit 与 SHA-256，不使用旧文档中的动态值。
- 当前候选版本变更边界已整理到 [CHANGELOG.md](CHANGELOG.md)；精确候选提交、安装器 SHA-256 和中继镜像来源只以同批 manifest/readiness/审计报告为准，尚未形成正式发布 tag。
- 当前 `main` 已推送直连诊断、回落改动、DirectLan 回环夹具、近期画质窗口、一键恢复动作以及签名候选 300 秒 soak 门禁；尚无本地 `v*` 发布 tag。

</details>
