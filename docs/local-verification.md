# 分层本地验证

更新日期：2026-10-26。入口是 `python scripts/verify.py`（Python 3.9+，macOS 通常使用 `python3`）；PowerShell 的 `scripts/verify.ps1` 与 POSIX 的 `scripts/verify.sh` 转发到同一实现。从任意工作目录调用脚本都使用它所在的 checkout，不依赖终端当前目录。

| 层 | 默认任务 | 证明范围 |
| --- | --- | --- |
| `fast`（默认） | 三个 Bun 项目 frozen 安装、类型检查、测试；Python 脚本测试；Rust fmt 与 workspace lib 测试 | 本地领域规则、现有契约和脚本行为；首次 Rust 编译仍可能较慢 |
| `integration` | 前端生产构建、Rust check/Clippy、独立 protocol-wire、protocol/video/session/crypto 契约、localhost transport/relay、direct-probe、FFI runtime、媒体恢复、Windows 全套/独立 host runtime 契约 | 编译、codec、实际runtime分别记录；Windows 专有任务在其他平台 skipped；编译通过不能覆盖业务失败 |
| `native` | DL2-025 生成 ABI 头只读同步检查、Apple Silicon FFI build、AppleCore Swift 测试、macOS 应用 Swift 测试；Windows 交互桌面捕获/编码 | 头检查跨平台可跑，依赖 Apple 分支 `5691aba` 的生成器；其余依赖平台、SDK、权限、设备，捕获与可能访问原生输入 API 的任务须显式 `--allow-device` |
| `perf` | 本机加密媒体 hardware soak；DL2-014 离线合成基线入口与完整性能验收缺口 | 离线基线依赖 `codex/dl2-windows-media` 的 `254f8be`，源文件未集成时 skipped；soak 只检查本机持续媒体与光标，完整同硬件前后比较仍 skipped |

任务成功只表示命令成功。已知测试 runner 必须报告至少一项测试；例如 Rust `--exact` 过滤器零匹配虽然退出 0，入口仍将其记为失败。报告的 `tests_reported` 是命令输出计数，不能累加为去重覆盖率；runner 内的 ignored/skipped 数量保留在原始日志，不能由一次命令成功推导所有功能已验收。当前注册的本地测试不访问托管中继/线上服务，也不调用发布入口。旧的 `verify-windows-resilience.py` 有托管中继探针，仍是单独的显式验收入口。

## 工具与依赖

Bun 固定 **1.3.14**，与三个 `packageManager` 一起校验；默认使用 PATH 中的 Bun。可用 `--bun` 指定已有的固定版本可执行文件；该目录会置于子进程 PATH 首位。系统默认版本不同会失败，入口不会自动升级工具。

如需复用 npm 的固定版本缓存，Windows 可这样解析实际可执行文件，再运行统一入口：

```powershell
$pinnedBun = (& npm exec --yes --package bun@1.3.14 -- bun -e 'console.log(process.execPath)').Trim()
python scripts/verify.py fast integration --bun $pinnedBun --target-dir E:\desk-link\.worktrees\dl2-targets\engineering
```

仅将 `python` 放在 `npm exec` 后面不保证子进程解析到固定 Bun；应使用上面解析出的实际路径。

macOS（Apple Silicon，已安装仓库 Rust toolchain 和 Xcode/Swift）：

```sh
python3 scripts/verify.py fast integration
python3 scripts/verify.py --task apple-core
```

Rust 版本必须匹配 `rust-toolchain.toml`，Cargo test/build/clippy 都使用 `--locked`。Windows 全部 Cargo 命令经 `scripts/run-windows-cargo.py` 准备原生工具。`--target-dir` 优先于 `CARGO_TARGET_DIR`，否则使用当前 checkout 的 `target`；Swift linker 同时指向选中 target 的 FFI 静态库，无须复制到另一个 checkout。安装依赖仅使用三个已有 lockfile 的 `bun install --frozen-lockfile`。

## 选择任务与证据

```sh
python scripts/verify.py all --list
python scripts/verify.py --task transport-localhost --task ffi-runtime
python scripts/verify.py --task direct-probe
python scripts/verify.py --task rust-check --task protocol-wire --task host-runtime-contract
python scripts/verify.py --task controller-loopback
python scripts/verify.py --task ffi-header-sync
python scripts/verify.py native perf --report dist/verify/device-status.json
```

`--task` 可重复，自动加入去重后的前置任务；不能同时指定层。`--list` 只输出计划，不安装、不执行、不生成通过报告。专用验收设备经操作人员授权后可使用 `--allow-device`；它允许捕获桌面和平台原生测试，不代表具备真实双机验收结果。`--soak-seconds` 为 10～300 秒，默认 30；不得把这种短 soak 当作数小时长稳。

默认报告 `dist/verify/report.json`，每个任务的 stdout/stderr 独立保存在同目录的 `report-logs/`。报告记录源码 commit/dirty、平台、Python/Bun/Cargo/rustc/Swift 实际版本、Rust toolchain 配置、target、命令、cwd、耗时、返回码、失败或跳过原因；每完成一个任务立即原子写入报告。可用 `--report` 保留各次执行结果。`--timeout` 控制每个子进程时限；超时/中断会清理 wrapper 与其子进程后返回失败。若进程树清理失败，后续任务全部 blocked，须先检查残留进程。

| 退出码 | 含义 |
| --- | --- |
| `0` | 所选任务与前置任务全部成功 |
| `1` | 工具版本、执行、超时或其他检查失败；独立任务仍继续 |
| `2` | 存在 skipped/blocked，所选范围没有完整验证，`passed=false` |
| `130` | 用户中断 |

缺少 Apple 主机、交互设备、权限或 DL2-014 性能基线时，报告明确 skipped，不能写作 pass。当前注册测试不声称已覆盖所有 DL2 修复：TS→Rust 滚轮边界、AppKit IME 实际接线、拖放坐标、拓扑变化、慢盘和所有恢复代数仍须各编号提供真实回归，再加入本入口；FFI Rust runtime 测试不能替代 Swift/Xcode 或双机证明。统一入口本身的合成回归位于 `scripts/tests/test_verify.py`。

`ffi-header-sync` 在生成器尚未集成时明确 skipped；集成 `5691aba250770d3c781c472966263a40ec0018de` 后只调用 `sync-ffi-header.py --check`，不自动改写头文件。它证明生成声明同步，真实 `abi_layout` 用例会随 FFI 的 `--tests` 任务执行；两者不能替代 Swift 调用、缓冲区所有权、错误恢复或设备验收。

## 追加跨层门禁：input epoch 与独立 probe

严格几何绑定的共享契约由 Apple/FFI 单一 owner 交付，仍属未发布协议 13 开发线。来源 `bc6c0a62d3817fbe44759cddf5069c3dee117f4c` 已复用为 `5501aad9f7d160f751048036ac2a14cf0a315680`：`VideoConfig` 在 `stream_id` 后有非零 `input_epoch: u64`；`InputEnvelope` 在 `timestamp_us` 后有非零 `stream_id: u64`、`input_epoch: u64`，并有共享 `InputContext` 类型。input codec 已统一保留前缀 `[0, PROTOCOL_VERSION:u16 little-endian]` 和 exact 解码；平台/transport 不手工拼前缀、不 fallback 无context格式。显式 `send_input_with_context` API 随最小桥接b473498复用为56f53b4，本地加密loopback5项通过；**生产调用迁移、host和呈现接线仍待完成，不得在发送时补当前值。** 以下不是整链已通过记录。

| 门禁 | 必须从生产边界观察到的结果 | 执行层/证据边界 |
| --- | --- | --- |
| context 非零、前缀与完整传递 | codec/ABI/Swift/TS 真实往返保留 stream/epoch；任一值为零、缺失或错代不得默认补当前上下文；验证 seq/timestamp 有效但 context 过期的输入，并拒绝旧合法 Function(1) 等包误解为新 MouseButton、错误/截断前缀和无 context fallback | protocol/FFI/平台桥的实际调用回归；由 owner 的 codec 合同证明新前缀与旧合法非零 sequence 不相交；字段名正则匹配不提供证明 |
| 排队旧事件与旧 release | 冻结发送器，在旧 epoch 入队后切换；正常输入、key-up/ReleaseAll 均保留原 context 并被 host 拒绝；新 epoch 按下的同一键不会被旧 release 释放 | 发送队列与记录型 host injector；仅本机 topology 代数或丢普通 press、保留所有 release 不满足门禁 |
| 首帧实际呈现前禁用 | 仅收到 VideoConfig、解码完成但 presented 尚未发生都不能启用输入；旧 stream/epoch 的迟到呈现不能启用当前 context；事件在入队时已固定 context | Windows 合成宿主、实际 WebView2 分别记录；Apple 原生 presented handler 必须另有真机回调结果 |
| 持久stream与新Noise/重启 | 同一持久stream下重连、重启、新审批与同进程新会话都重新OsRng非零起点；旧plaintext即便被新Noise重加密也不匹配，旧release/呈现回调不生效；新随机epoch可更小，不能跨会话按大小判断 | HostConnectionSettings确实持久化stream；待host/生产者实际接线，不能用新Noise本身或固定1作新鲜度证明 |
| RNG与推进边界 | 注入RNG失败、零值、重复base与u64溢出，观察明确失败/上下文重建，不默认1、wrap、round/clamp；仅几何/活动输出/有效性边界checked推进，纯编码保持epoch/持键 | 可控随机源/映射竞争回归；随机epoch只绑输入新旧，不是key/nonce；这些尚未由sender原语实现 |
| config_version与epoch配对 | 同尺寸换屏、虚拟原点/映射改变或有效性变化也推进真实config_version、发布配置/恢复关键帧；纯编码可只变version并保持epoch/不清键；旧buffered输出不重贴身份 | 当前帧/mailbox无独立epoch。已有同stream+同version但任意配置字段变化→InconsistentVideoConfig，须保留并回归；旧帧误激活是绕过该拒绝后的风险，不是当前复现结果 |
| 对应帧才确认新epoch | 旧version变epoch明确拒绝，正确新version配置与该帧实际呈现后才放行；迟到旧缓冲/解码帧、同尺寸帧或旧token不能借新配置启用输入 | token来自帧真实配置和会话代数；不只测尺寸变化，也不放宽一致性校验来接通 |
| Windows完整u64 IPC | 实际JSON/IPC→TS队列→Rust往返2^53−1、2^53、2^53+1、u64::MAX的规范十进制字符串，以BigInt保持/比较身份；拒绝number、零context、负值、前导零、空白、指数和越界 | 当前为产品接线规则；C/Swift以uint64_t/UInt64做相同边界回归，不把double或round/clamp当修复 |
| host 原子清键/推进/映射 | 在受控 injector 中阻塞切换竞争，观察清键、epoch 推进、映射发布和锁内核对形成同一边界；旧坐标不能落到新屏 | Windows host 本地集成与真实切屏分开；不以锁字段存在或 config_version 相同推导正确 |
| 取消独立生效 | 普通输入队列满/发送背压、旧输入待丢弃、新首帧未呈现时，用户取消与 host 本机收回仍可停止/清理；不能先要求有效远端 context 或等待普通队列空位 | 合成背压/取消/迟到回调；实际权限/设备另记 |
| 发送调用完整迁移 | 清点 ControllerRuntime 旧 send_input(event) 的所有生产调用，不只鼠标：普通键盘/指针/滚轮、Text、粘贴组合键、排队release、断开与丢包释放；context/pressed ledger和C/Swift布局来自同一owner声明 | Rust生产调用清单、context队列/加密loopback与ABI/C布局；底层QuicClient::send_input(bytes)不是同一个待迁移API，不能按函数名盲改 |
| probe 重放窗隔离 | 双向慢 probe 经 256 个视频包后仍能认证；probe 重放和跨 lane 解密仍拒绝；真实 shared endpoint 等待可取消且不持媒体 crypto 锁 | 已复用的独立提交 `0836a8b` 与 shared-probe 前置；`--task direct-probe` 跑 crypto/transport 的 `--lib --tests --locked`，不证明双机/WAN p95/p99 |

生产语义和调用迁移必须在真实契约及 host/UI 接线位于**同一 revision** 后执行。目前 HostVideoPipeline、FFI host 配置和两处controller输入生产者仍显式零；codec会拒绝，当前不可交付。不得默认1、跳过业务失败或在发送时补当前epoch。现有codec或probe任务通过不关闭其他门禁；DL2-013 严格绑定继续保持 `[ ]`，seq、wallclock、仅 config_version 都不能替代它。

probe 复用链与工程源码快照见[能力/版本记录](current-capabilities.md)：原 lane 0～7 不重排，仅追加 `SecureLane::DirectProbe = 8` 并更新 COUNT/ALL；未改 JOIN 版本或 QUIC 事件槽数量。本分支协议 13 与 main 基线协议 12 分别记录，来源分支的 79 项报告不代替本工程接入测试。双机黑洞、慢候选、半包下输入/画面 p95/p99 和锁持有分布仍须另验。

此前 probe 接入报告 `dist/verify/probe-integration.json` 为 passed：crypto/transport 79、workspace lib 259、protocol/video/session/crypto 109、relay 27、FFI runtime 39、媒体恢复 2、Windows 契约 131、Python 134 项通过，fmt 通过；计数有重叠，不能相加为独立覆盖率。`probe-clippy.json` 的 frozen 前端准备/生产构建及 workspace 全 target/feature Clippy 通过。两份报告记录旧源码 `4c3648a`、dirty=true，不能证明下述过渡源码可用。

合同导入后的实际报告 `dist/verify/input-context-transition.json` 记录源码 `5501aad`、dirty=true：`rust-check` 编译通过；`protocol-wire` **47项**通过（来源owner报告49，本次执行数不能照搬）；`direct-probe`79项通过；`host-runtime-contract`9通过/1失败，真实生产 `HostVideoPipeline::prepare` 构造epoch=0，codec返回 `Protocol(InvalidVideoConfig)`。整体 **failed / passed=false**，未skip或改断言；这是尚未接线的业务失败。`rust-check` 只证明编译，codec正反例也不替代host/UI生产路径。没有真实远控、注入、Apple presented或双机结果。

### 显式发送 API 的生产迁移清单

以下清单在 `5501aad` 核对，桥接 `56f53b4` 后七处生产调用仍原样存在（迁移的两处测试调用不在生产清单中）。CodeGraph 发现入口后逐项核对实际 receiver 类型，旧 `ControllerRuntime::send_input(event)` 仍走零context拒绝方法；不能把重名的底层 `QuicClient::send_input(bytes)` 作为同一个API盲改。

| 生产路径 | 当前直接调用/上游 | 迁移必须覆盖 |
| --- | --- | --- |
| Windows 普通输入 | `apps/windows-ui/src-tauri/src/controller.rs:2594`；Manager.send_input→ControllerCommand::Input | 键盘、鼠标移动/按键、滚轮与前端合成释放；入队即固定 context |
| Windows Text/IME | 同文件 `send_text_input:4583,4590`，由 Text 命令调用 | 字符/换行/Tab的 down/up 两处都带原 context，不能只迁鼠标 |
| Windows 粘贴 | 同文件 `send_remote_paste_shortcut:4617`，剪贴板成功回执 `:3139` 后触发 | 粘贴意图、等待回执、Ctrl+V down/up 使用捕获的同一 context，不在回执到达时取最新值 |
| FFI 普通输入 | `crates/desklink-ffi/src/worker.rs:466` | Apple键鼠/Unicode队列保留context |
| FFI 优先/延迟释放 | 同文件 `:439,456` | 重连待释放与ReleaseAll队列均保留按下时context，旧release不影响新epoch |

ReleaseAll还要回溯 Windows `controller.ts::releaseRemoteInput` 的普通释放展开及 `fireInput`，Apple `ControllerBridge.releaseAll`→C release API→FFI pressed ledger，以及 reject/destroy 的本机清理路径；远端过期release与用户取消/本机收回分开验证。`controller-loopback` 单独跑加密localhost controller测试，使其他FFI unit失败不会掩盖该二进制的实际结果；它不替代上表生产接线、ABI/C布局或真机呈现。

桥接前原语测试 `controller-context-transition.json` 为 failed / passed=false（源码 `f6ff20b`、dirty=true）：3通过/2失败，两处旧 send_input 调用返回 `Protocol(InvalidInput)`；后台 Closed 是其后关闭的结果，不能误报外部服务不可用。保留原始失败记录，不用后续结果覆盖旧源码身份。

完整 FFI 切片 `9ad7154b365de875d725188cfa091f5c21da0b5a` 依赖 Apple 003/004/025 的错误ABI、send_cancellable、生成头和布局测试，本工程未pick整笔。已复用产品独立桥接 `b473498382bd163f6085c019521105741264707c` → `56f53b4a17347714a91a2cb269bc5addc5777916`，唯一新增前置是已复用的bc6合同。controller.rs/test两文件增量与9ad的稳定patch-id均为 `2f004b5c19baaa5f3df4360b605818df1f40f9b4`，后续Apple整合只保留一份等价API/测试，不重复pick或覆盖整文件。其余C/FFI ledger/worker/Swift仍以Apple源分支为准；新增C字段与Swift布局必须独立验证，来源报告34项不算本工程结果。

桥接后实际 `captured-context-bridge.json` 来自干净源码 `56f53b4`、dirty=false：编译、protocol47和 controller加密loopback5项通过、0忽略；host-runtime-contract仍9通过/1失败（零epoch配置），整体failed / passed=false。该API原语成功不能关闭七处生产调用、Text/粘贴/ReleaseAll、host映射或呈现门禁。`captured-context-bridge-tooling.json` 在干净源码 `31e9fb0` 的Python134项与fmt通过，证明工具/格式，不替代业务失败。

### 有序InputSender的局部通过与完整失败

最小独立来源 `3ea72dc7f5a66d4cd96c197c0aadb7603cee2436` 已复用为 `d6f153ed59c6d91b7cfa7337dd1b1a9fbeae9cf6`，前置仅bc6+b473。Runtime.input_sender返回可clone的ControllerInputSender，二者共用send_session_input及唯一序号；输入专用序号锁持完整wire写，共享crypto锁在网络等待前释放。它不是新的C/Swift/worker接线，后续保留一份Runtime实现并去重b473/9ad等价来源，不能整文件覆盖Sender扩展。

本工程 `ordered-input-sender.json` 记录干净源码d6f153e、dirty=false：fmt/编译/protocol47通过，独立controller-loopback **6通过、0忽略**。新增真实加密回归收到32并发clone加Runtime共36帧，seq严格1..36、原context保留；零context/旧无context入口不发包也不消耗序号。它证明“不把旧context补成最新值”，不证明host会拒绝旧context或新Noise已分配新epoch。

同次完整 `ffi-runtime` 中lib12、ABI7、platform1、controller6、directory4通过，旧Host **8通过/2失败**；Windows host-runtime-contract **9通过/1失败**，仍为零epoch配置未接通。整体 **failed / passed=false**。选择lib或loopback的通过不能改写完整FFI失败，不能跳过这两个Host测试后宣称全通过；新鲜度、config配对、IPC、Text/粘贴/release生产路径与真实presented继续按上表验收。

### Windows joint证据入口（尚待交付）

产品组已被安排在独立 `codex/dl2-windows-joint` 从产品固定快照 `16594c4f8780cd565afd57695f88e29277ec5939` 整合媒体固定快照 **`89bd1e5161653dd668d290a417bd5f6cc146290a`**。媒体先前快照为 `fb9734499fc560af79b11709aae185eb4ddcc81a`；若joint已整合旧快照，追加89并保留已解冲突，不重置或覆盖原WIP。本工程未操作该joint worktree。

产品源交接路径 `E:/desk-link/.worktrees/dl2-targets/windows-product/evidence/handoff-16594c4.json` 与媒体源分支测试都只证明各自快照，不等于联合通过。当前尚未收到joint最终SHA/报告，因此不据安排、merge完成或源分支各自通过更新041/042为联合通过。

收口须同时取得：joint最终完整SHA及dirty/来源记录、逐问题历史/等价patch去重说明、实际Bun/原生/跨层命令和原始报告，以及上表各epoch/config/IPC/呈现/取消门禁对应的运行结果。失败、忽略与缺设备条件分别保留，不能把选中用例通过扩大成全部通过。FFI Mac Host、Swift/真实Mac presented、实际WebView与双机验收继续单列；Windows joint不能代替这些证据，也不会使本工程旧源码上的失败报告失效。
