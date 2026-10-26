# 分层本地验证

更新日期：2026-10-26。入口是 `python scripts/verify.py`（Python 3.9+，macOS 通常使用 `python3`）；PowerShell 的 `scripts/verify.ps1` 与 POSIX 的 `scripts/verify.sh` 转发到同一实现。从任意工作目录调用脚本都使用它所在的 checkout，不依赖终端当前目录。

| 层 | 默认任务 | 证明范围 |
| --- | --- | --- |
| `fast`（默认） | 三个 Bun 项目 frozen 安装、类型检查、测试；Python 脚本测试；Rust fmt 与 workspace lib 测试 | 本地领域规则、现有契约和脚本行为；首次 Rust 编译仍可能较慢 |
| `integration` | 前端生产构建、Rust check/Clippy、独立 protocol-wire、protocol/video/session/crypto 契约、localhost transport/relay、direct-probe、FFI runtime、媒体恢复、Windows 全套/独立 host runtime 契约 | 编译、codec、实际runtime分别记录；Windows 专有任务在其他平台 skipped；编译通过不能覆盖业务失败 |
| `native` | DL2-025 生成 ABI 头只读同步检查、Apple Silicon FFI build、AppleCore Swift 测试、macOS 应用 Swift 测试；Windows 交互桌面捕获/编码 | 当前候选已包含 Apple `5691aba` 来源的生成器，头检查跨平台可跑且已通过；其余依赖平台、SDK、权限、设备，捕获与可能访问原生输入 API 的任务须显式 `--allow-device` |
| `perf` | 本机加密媒体 hardware soak；DL2-014 离线合成基线入口与完整性能验收缺口 | `254f8be` 来源的CPU离线基线已随媒体历史集成并实际通过；旧源码尚未集成时明确 skipped。soak只检查本机持续媒体与光标，完整同硬件前后比较仍未完成 |

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

Rust 版本必须匹配 `rust-toolchain.toml`，Cargo test/build/clippy 都使用 `--locked`。Windows 全部 Cargo 命令经 `scripts/run-windows-cargo.py` 准备原生工具；完整MSVC developer环境修复已提交为 `32e13e27f4e42d23ca074c7144590a4548c80ccd`，当前候选实际执行cl的C布局用例通过，不只是Rust编译或环境模拟。`--target-dir` 优先于 `CARGO_TARGET_DIR`，否则使用当前 checkout 的 `target`；Swift linker 同时指向选中 target 的 FFI 静态库，无须复制到另一个 checkout。安装依赖仅使用三个已有 lockfile 的 `bun install --frozen-lockfile`。

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

缺少 Apple 主机、交互设备、权限或完整 DL2-014 性能条件时，报告明确 skipped，不能写作 pass。第一波已纳入TS/Rust滚轮、拖放归属、拓扑、慢盘隔离与恢复等适用软件回归，仍不声称覆盖所有DL2验收；AppKit IME实际行为、真实拓扑/慢盘/双机及所有恢复场景按各编号继续补证。FFI Rust runtime测试不能替代Swift/Xcode或双机证明。统一入口本身的合成回归位于 `scripts/tests/test_verify.py`。

`ffi-header-sync` 在旧源码生成器尚未集成时明确 skipped；当前候选已集成 `5691aba250770d3c781c472966263a40ec0018de` 来源的生成器，只调用 `sync-ffi-header.py --check`，不自动改写头文件，实际双头检查通过。真实 `abi_layout` 用例随 FFI 的 `--tests` 任务执行，当前已通过MSVC C布局检查；两者不能替代Swift调用、缓冲区所有权、错误恢复或设备验收。

## 第一波协议13的已执行结果

已测试的干净源码为 `a4c5b404d43794d646394923a481f23c9298c3ed`，v3共25项软件检查全部通过。它完整保留J `5782341`、Apple `f954a68`与input admission后继 `cb7514c`、网络 `4b155615`、工程 `6b31360`、GUI `50691585`、诊断producer `28c11ee`与services `d2d4f1c`，含已提交的MSVC环境修复与relay资源/生命周期集成门禁。完整来源、命令、原始报告和证据绑定见[第一波集成报告](wave1-integration.md)。这不是未提交工程切片，也不是只核对各来源分支报告。

| 已执行的软件范围 | 结果与边界 |
| --- | --- |
| Python脚本/合同 | 257通过 |
| Bun前端 / 账号 / 诊断 | 474 / 79 / 129通过，固定Bun1.3.14；类型检查与前端生产构建纳入软件门禁 |
| 完整FFI lib/tests | 91通过、0失败、0忽略：42 lib、7 ABI、2真实MSVC C布局、1平台、6 controller、5 directory、28 host |
| Windows | 292通过、17忽略；包括保留的输入/媒体/传输回归与Windows joint两项软件MFT/QUIC/Noise联合用例，忽略不计通过 |
| relay资源与生命周期 | 64通过、3忽略；完整软件门禁不只运行旧relay lib选择集 |

另从当前native producer实际重新生成fixture，19个案例经真实本地HTTP→验签/schema→文件SQLite→重开/去重按预期通过。正例直接使用原始RustSignedRequest的body/hash/签名，不以手写JSON、改写或重签替代；范围为7事件、24个event/field对、18个不同u64键。保留历史safe数字兼容，旧unsafe numeric整批拒绝，篡改后返回401；案例通过不表示所有请求均应接收。原始签名请求与实际存储/拒绝结果均按[集成报告](wave1-integration.md)绑定，不等同真实设备诊断送达或生产部署。

独立platform native/perf报告为2通过、7跳过，整体incomplete / passed=false；通过项仅双头检查与CPU离线基线。以下仍未完成：

- [ ] Swift/Xcode、六项Swift Recording及真实Apple presented/输入行为。
- [ ] 实际WebView、原生输入注入、权限、双机和真实显示拓扑，包括Apple同stream完整源拓扑/epoch权威映射。
- [ ] 完整Mac功能、GPU/硬件成功路径、同条件性能对照、长稳与正式发布/安装签名验收。

第一波软件候选通过不表示这些跳过项通过，也不表示已合main或正式release；main仍为协议12。GPU235后续独立候选、文件协议14与LAN024后续生产接线不合入第一波，不阻塞本轮已完成的软件检查。以下旧工程/J来源报告保留各自源码和失败结果，不再作为当前候选零producer或Apple未合入的结论。

## 第二波GPU独立候选

第一波冻结c3及其a4测试源不变。第二波干净源码 `97ff7c08199cdad4511b0044ddf783d79e2295b1` 从c3完整merge GPU235后，实际重新通过fast/integration的25检查：workspace lib516通过/18忽略、Windows tests297通过/32忽略，FFI91/0、relay64/3；Bun474/79/129和Python257通过。六条精确GPU命令合24通过/0忽略，其中15项在默认套件通常ignored，9项常规（与软件套件有重叠），不能把默认32忽略都写成未跑或把范围直接相加。

新native fixture→原body/签名→本地HTTP/SQLite重开、去重及分析19案例通过。平台native/perf仍2通过/7跳过、incomplete，CPU离线基线encode=false；合成GPU编码/解码验证不是正式桌面、设备恢复、真实性能或Swift验收。原始报告、精确过滤器和剩余范围见[第二波集成报告](wave2-gpu-integration.md)。

## 追加跨层门禁：input epoch 与独立 probe

严格几何绑定的共享契约由Apple/FFI单一owner交付，仍属未发布协议13开发线。历史来源 `bc6c0a62d3817fbe44759cddf5069c3dee117f4c` 复用为 `5501aad9f7d160f751048036ac2a14cf0a315680`：`VideoConfig`在stream_id后有非零input_epoch:u64，`InputEnvelope`在timestamp_us后有非零stream_id/input_epoch:u64，并有共享InputContext类型。input codec统一保留前缀 `[0, PROTOCOL_VERSION:u16 little-endian]` 和exact解码，平台/transport不手工拼前缀、不fallback无context格式。最小显式 `send_input_with_context` 桥接b473498→56f53b4的本地加密loopback5项通过；**仅在当时5501aad/56f53b4/d6f153e工程阶段，生产调用、host和呈现尚未全部接线。** 当前a4已完整聚合J/A/CB并完成适用软件检查，不得在发送时补当前值；下表继续定义生产合同和设备验收边界。

| 门禁 | 必须从生产边界观察到的结果 | 执行层/证据边界 |
| --- | --- | --- |
| context 非零、前缀与完整传递 | codec/ABI/Swift/TS 真实往返保留 stream/epoch；任一值为零、缺失或错代不得默认补当前上下文；验证 seq/timestamp 有效但 context 过期的输入，并拒绝旧合法 Function(1) 等包误解为新 MouseButton、错误/截断前缀和无 context fallback | protocol/FFI/平台桥的实际调用回归；由 owner 的 codec 合同证明新前缀与旧合法非零 sequence 不相交；字段名正则匹配不提供证明 |
| 排队旧事件与旧 release | 冻结发送器，在旧 epoch 入队后切换；正常输入、key-up/ReleaseAll 均保留原 context 并被 host 拒绝；新 epoch 按下的同一键不会被旧 release 释放 | 发送队列与记录型 host injector；仅本机 topology 代数或丢普通 press、保留所有 release 不满足门禁 |
| 首帧实际呈现前禁用 | 仅收到 VideoConfig、解码完成但 presented 尚未发生都不能启用输入；旧 stream/epoch 的迟到呈现不能启用当前 context；事件在入队时已固定 context | Windows 合成宿主、实际 WebView2 分别记录；Apple 原生 presented handler 必须另有真机回调结果 |
| 持久stream与新Noise/重启 | 同一持久stream下重连、重启、新审批与同进程新会话取得新的非零随机起点/checked分配context；旧plaintext即便被新Noise重加密也不匹配，旧release/呈现回调不生效；新随机epoch可更小，不能跨会话按大小判断 | HostConnectionSettings确实持久化stream；当前J/A已有host/生产者接线及新Noise软件回归，不能用新Noise本身或固定1作新鲜度证明，真实设备生命周期另验 |
| RNG与推进边界 | 注入RNG失败、零值、重复base与u64溢出，观察明确失败/上下文重建，不默认1、wrap、round/clamp；仅几何/活动输出/有效性边界checked推进，纯编码保持epoch/持键 | 当前host可控随机源/映射竞争软件回归与sender原语分别记录；随机epoch只绑输入新旧，不是key/nonce，不能以sender测试单独证明host规则 |
| config_version与epoch配对 | 同尺寸换屏、虚拟原点/映射改变或有效性变化也推进真实config_version、发布配置/恢复关键帧；纯编码可只变version并保持epoch/不清键；旧buffered输出不重贴身份 | 当前帧/mailbox无独立epoch。已有同stream+同version但任意配置字段变化→InconsistentVideoConfig，须保留并回归；旧帧误激活是绕过该拒绝后的风险，不是当前复现结果 |
| 对应帧才确认新epoch | 旧version变epoch明确拒绝，正确新version配置与该帧实际呈现后才放行；迟到旧缓冲/解码帧、同尺寸帧或旧token不能借新配置启用输入 | token来自帧真实配置和会话代数；不只测尺寸变化，也不放宽一致性校验来接通 |
| Windows完整u64 IPC | 实际JSON/IPC→TS队列→Rust往返2^53−1、2^53、2^53+1、u64::MAX的规范十进制字符串，以BigInt保持/比较身份；拒绝number、零context、负值、前导零、空白、指数和越界 | 当前J已有生产接线和完整u64软件回归；C/Swift使用uint64_t/UInt64，C ABI已实际通过，Swift运行仍未执行，不把double或round/clamp当修复 |
| host 原子清键/推进/映射 | 在受控 injector 中阻塞切换竞争，观察清键、epoch 推进、映射发布和锁内核对形成同一边界；旧坐标不能落到新屏 | Windows host 本地集成与真实切屏分开；不以锁字段存在或 config_version 相同推导正确 |
| 取消独立生效 | 普通输入队列满/发送背压、旧输入待丢弃、新首帧未呈现时，用户取消与 host 本机收回仍可停止/清理；不能先要求有效远端 context 或等待普通队列空位 | 合成背压/取消/迟到回调；实际权限/设备另记 |
| 发送调用完整迁移 | 清点 ControllerRuntime 旧 send_input(event) 的所有生产调用，不只鼠标：普通键盘/指针/滚轮、Text、粘贴组合键、排队release、断开与丢包释放；context/pressed ledger和C/Swift布局来自同一owner声明 | Rust生产调用清单、context队列/加密loopback与ABI/C布局；底层QuicClient::send_input(bytes)不是同一个待迁移API，不能按函数名盲改 |
| probe 重放窗隔离 | 双向慢 probe 经 256 个视频包后仍能认证；probe 重放和跨 lane 解密仍拒绝；真实 shared endpoint 等待可取消且不持媒体 crypto 锁 | 已复用的独立提交 `0836a8b` 与 shared-probe 前置；`--task direct-probe` 跑 crypto/transport 的 `--lib --tests --locked`，不证明双机/WAN p95/p99 |

生产语义和调用迁移必须在真实契约及host/UI接线位于**同一revision**后验证。当前a4已保留J的非零Windows host/producer、完整A/CB的FFI与Apple接线及呈现/入队合同，相关软件门禁通过。HostVideoPipeline、FFI host配置和controller生产路径显式零、codec拒绝的记录仅属于下述5501aad/56f53b4/d6f153e历史工程源码。不得默认1、跳过业务失败或在发送时补当前epoch。DL2-013整体验收仍 `[ ]`，缺项为实际WebView、Swift/Xcode/Recording/真实presented、输入/双机/拓扑等；seq、wallclock、仅config_version不能替代输入context，旧远端release拒绝也不能阻止用户取消/Host本机收回独立生效。

probe复用链与工程源码快照见[能力/版本记录](current-capabilities.md)：原lane0～7不重排，仅追加 `SecureLane::DirectProbe = 8` 并更新COUNT/ALL；未改JOIN版本或QUIC事件槽数量。当前候选协议13与main基线协议12分别记录；旧来源79项报告不代替a4接入报告，当前统一入口已实际通过相应软件门禁。双机黑洞、慢候选、半包下输入/画面p95/p99和锁持有分布仍须另验。

### 历史工程报告：保留原源码的通过与失败

此前 probe 接入报告 `dist/verify/probe-integration.json` 为 passed：crypto/transport 79、workspace lib 259、protocol/video/session/crypto 109、relay 27、FFI runtime 39、媒体恢复 2、Windows 契约 131、Python 134 项通过，fmt 通过；计数有重叠，不能相加为独立覆盖率。`probe-clippy.json` 的 frozen 前端准备/生产构建及 workspace 全 target/feature Clippy 通过。两份报告记录旧源码 `4c3648a`、dirty=true，不能证明下述过渡源码可用。

合同导入后的历史报告 `dist/verify/input-context-transition.json` 记录源码 `5501aad`、dirty=true：`rust-check`编译通过，`protocol-wire` **47项**通过（来源owner报告49，当次执行数不能照搬），`direct-probe`79项通过；`host-runtime-contract`9通过/1失败，真实生产 `HostVideoPipeline::prepare` 构造epoch=0，codec返回 `Protocol(InvalidVideoConfig)`。整体 **failed / passed=false**，未skip或改断言，这是该历史源码尚未接线的业务失败，不是a4现状。`rust-check`只证明编译，codec正反例不替代host/UI生产路径；该次没有真实远控、注入、Apple presented或双机结果。

### 历史显式发送API迁移清单（5501aad/56f53b4）

以下历史清单在 `5501aad` 核对，桥接 `56f53b4` 后当时七处生产调用仍原样存在（迁移的两处测试调用不在生产清单中）。CodeGraph发现入口后逐项核对实际receiver类型，旧 `ControllerRuntime::send_input(event)` 走零context拒绝方法；不能把重名的底层 `QuicClient::send_input(bytes)` 作为同一个API盲改。表中路径/行号绑定该旧工程快照，不是当前a4待迁移清单；第一波已随完整J/A/CB接通生产输入和队列。

| 历史生产路径 | 当时直接调用/上游 | 持续需要覆盖的输入类别 |
| --- | --- | --- |
| Windows 普通输入 | `apps/windows-ui/src-tauri/src/controller.rs:2594`；Manager.send_input→ControllerCommand::Input | 键盘、鼠标移动/按键、滚轮与前端合成释放；入队即固定 context |
| Windows Text/IME | 同文件 `send_text_input:4583,4590`，由 Text 命令调用 | 字符/换行/Tab的 down/up 两处都带原 context，不能只迁鼠标 |
| Windows 粘贴 | 同文件 `send_remote_paste_shortcut:4617`，剪贴板成功回执 `:3139` 后触发 | 粘贴意图、等待回执、Ctrl+V down/up 使用捕获的同一 context，不在回执到达时取最新值 |
| FFI 普通输入 | `crates/desklink-ffi/src/worker.rs:466` | Apple键鼠/Unicode队列保留context |
| FFI 优先/延迟释放 | 同文件 `:439,456` | 重连待释放与ReleaseAll队列均保留按下时context，旧release不影响新epoch |

ReleaseAll持续覆盖Windows `controller.ts::releaseRemoteInput`的普通释放展开及fireInput、Apple `ControllerBridge.releaseAll`→C release API→FFI pressed ledger，以及reject/destroy本机清理；远端过期release与用户取消/本机收回分开验证。`controller-loopback`单独跑加密localhost controller测试，使其他FFI unit失败不会掩盖该二进制的实际结果；当前a4已完成完整FFI与适用软件检查，但该单独loopback仍不能替代生产接线、ABI/C布局或真机呈现。

桥接前原语测试 `controller-context-transition.json` 为 failed / passed=false（源码 `f6ff20b`、dirty=true）：3通过/2失败，两处旧 send_input 调用返回 `Protocol(InvalidInput)`；后台 Closed 是其后关闭的结果，不能误报外部服务不可用。保留原始失败记录，不用后续结果覆盖旧源码身份。

完整FFI切片 `9ad7154b365de875d725188cfa091f5c21da0b5a` 依赖Apple003/004/025的错误ABI、send_cancellable、生成头和布局测试；56f53b4阶段本工程未pick整笔，仅复用产品桥接 `b473498382bd163f6085c019521105741264707c` → `56f53b4a17347714a91a2cb269bc5addc5777916`，唯一新增前置为bc6合同。controller.rs/test两文件增量与9ad的稳定patch-id均为 `2f004b5c19baaa5f3df4360b605818df1f40f9b4`。后续第一波已真实合入完整f954/CB历史，并去重保留一份等价API/测试及Sender扩展，不整文件覆盖；C/FFI ledger/worker与Swift代码已随该历史整合，当前C ABI已实际通过，Swift布局/调用执行仍待Apple环境。此前来源34项只属于当时快照，不算当前候选或Apple最新结果。

桥接后的历史 `captured-context-bridge.json` 来自干净源码 `56f53b4`、dirty=false：编译、protocol47和controller加密loopback5项通过、0忽略；host-runtime-contract仍9通过/1失败（零epoch配置），整体failed / passed=false。该阶段API原语成功不能证明当时七处生产调用、Text/粘贴/ReleaseAll、host映射或呈现已完成。`captured-context-bridge-tooling.json`在干净源码 `31e9fb0` 的Python134项与fmt通过，证明当时工具/格式，不替代其业务失败；上述旧失败不覆盖a4已接线且通过的软件结果。

### 历史有序InputSender的局部通过与完整失败（d6f153e）

最小独立来源 `3ea72dc7f5a66d4cd96c197c0aadb7603cee2436` 复用为 `d6f153ed59c6d91b7cfa7337dd1b1a9fbeae9cf6`，前置仅bc6+b473。Runtime.input_sender返回可clone的ControllerInputSender，二者共用send_session_input及唯一序号；输入专用序号锁持完整wire写，共享crypto锁在网络等待前释放。该独立切片当时不含新的C/Swift/worker接线；第一波现已保留其Runtime/Sender扩展，并与完整Apple历史去重整合，不整文件覆盖。

本工程 `ordered-input-sender.json` 记录干净源码d6f153e、dirty=false：fmt/编译/protocol47通过，独立controller-loopback **6通过、0忽略**。新增真实加密回归收到32并发clone加Runtime共36帧，seq严格1..36、原context保留；零context/旧无context入口不发包也不消耗序号。它证明“不把旧context补成最新值”，不证明host会拒绝旧context或新Noise已分配新epoch。

同次完整 `ffi-runtime` 中lib12、ABI7、platform1、controller6、directory4通过，旧Host **8通过/2失败**；Windows host-runtime-contract **9通过/1失败**，当时仍为零epoch配置未接通。整体 **failed / passed=false**。这些是旧工程d6f153e尚缺完整Apple ABI/Host依赖的实际结果，不能外推为Apple当前源或a4候选失败；a4完整FFI91通过的结果另列在前。选择lib或loopback通过不能改写旧报告，也不能跳过Host测试宣称该旧源码全通过。当前新鲜度/config配对/IPC/Text/粘贴/release软件回归与未运行的真实presented继续分开记录。

### 历史Windows joint证据与第一波承接

第一波整合过程的首次完整FFI因真实C布局测试找不到PATH中的cl而失败，保留于 `merged-ffi.json`；加载完整MSVC developer环境后的 `merged-ffi-msvc.json` 实际记录FFI91通过/0失败/0忽略及双头检查通过，原J两项Host失败已在整合源码恢复。该中间复跑绑定a97f2d4加当时未提交环境切片，MSVC helper17项回归含真实cmd空格路径；这份历史身份不改成最终干净源。环境修复随后已提交32e13e2，services d2的consumer及原producer签名/本地HTTP/store/analysis现已合入并在干净a4完成全部软件检查，详见[第一波集成报告](wave1-integration.md)。Swift/Xcode/Metal、设备与真实上云送达仍未运行，不再将consumer代码交付或MSVC修复写成当前待办。

产品组在独立 `codex/dl2-windows-joint` 的原始安排为产品 `16594c4f8780cd565afd57695f88e29277ec5939` 整合媒体 `89bd1e5161653dd668d290a417bd5f6cc146290a`（更早为 `fb9734499fc560af79b11709aae185eb4ddcc81a`）。J578最终第一parent采用 **`27fec756ba97ee6f67228dc9530c7b0578a49ff3`**，本工程只读Git确认它直接接续16594、代码仅提取InputSendFuture类型别名；另一parent为媒体021修复 **`65c70fd658d0526a8a27b75fdbf120ae065222bf`**，直接parent为 `6dbe53d64c60bbdd50af4b592ebef7471b800fcc`。当时协调方报告开始按这两个parents真实remerge，要求保留已解冲突、审计resolution、联合测试与逐问题历史；该安排已由J578固定提交和后续a4候选承接，不再是当前正在merge的状态。本工程未操作原joint WIP。

产品源交接 `E:/desk-link/.worktrees/dl2-targets/windows-product/evidence/handoff-16594c4.json` 只证明旧16594快照；27fec提交TODO报告UI库135通过/1项签名资产忽略、本包 `--lib --tests --no-deps` Clippy通过，当时工程组未复跑，要求新parents整合后另做跨包严格检查。协调方当时另报10份人工resolution/新增联合测试备份及SHA256清单，工程组未读取或核验这些备份；不得把该来源报告当作当前候选执行证据。后续J578已固定tree与联合测试，当前a4又完成完整软件门禁，三阶段证据分别保留，不用本包或备份声明替代最终源码报告。

历史协调来源曾报告完整FFI71、Python186及双头通过、013 worker owned future进行中，但未提供对应完整测试SHA；该旧报告仍不绑定动态branch head。随后已取得固定Apple f954的FFI88/原生165/Python201来源及CBcb7514的FFI91/Python208后继，完整历史均已进入a4并实际联合复跑，不再等待Apple稳定SHA，也不单独flat-pick9ad。旧joint两个MacHost失败仅属于尚缺完整Apple链的J源码，不外推Apple当前或a4失败；当前Swift六项Recording、Swift/Xcode、真实Mac presented与设备验收仍未执行或未完成。

媒体65c冻结交接已读取 `E:/desk-link/.worktrees/dl2-targets/windows-media/validation/final-validation.json` 与同目录 `windows-media-handoff.json`，工程组核对三组原日志合计448通过/0失败/18忽略、source前后均65c、空working_status及固定Git对象Cargo.lock哈希一致；37笔唯一提交/subject/ancestry与handoff吻合，含三条imported contract。来源workspace/all-targets check、限定包lib/tests严格Clippy及workspace fmt通过，均保留来源身份，不冒称工程组当时复跑。65c仅主项020/046/050完成，015仍CPU读回、无GPU surface链，030仍256MiB/无目录、大文件和持久块证明；这些冻结源边界不扩展到后续GPU235/文件14，也不能单凭源冻结关闭041/013全部平台验收。65c现已通过J进入a4，第一波软件检查另有干净源码实跑证据。报告哈希和完整边界见[来源核对记录](second-stage-review-evidence.md)。

Windows joint固定来源 `5782341e22ba55b2efe30a8eeaeafbebfe0daaba` / tree `59c7270aea76bc1d542ac49c625e308b42ef20f2`，parents27fec+65c；工程组核对27产品/37媒体历史、477源码blob、已跟踪联合测试及六份原日志哈希。J源码当时Rust634通过/2失败/19忽略、Cargo101（两项FFI Host零epoch、尚未合完整Apple），八包lib/tests严格Clippy无no-deps及Bun1.3.14的474项/构建通过。完整范围见[固定证据核对](second-stage-review-evidence.md)。这是当时只读核对的来源结果，J报告本身仍不能改写为全绿；a4后续完整合入A/CB并通过FFI91，不能继续把J两项旧失败当成候选当前失败。

第一波独立候选已从J578真实合入完整Apple `f954a68153168a1ded048ddf93eb9c565684d096` 与后继 `cb7514cc29b7a94ad40ac00f0a5b98936b7d5be5`、网络4b155615、工程6b31360、GUI50691585、producer28c11ee及最终services d2d4f1c，保留J非零Windows producer并去重共享等价patch，不单挑9ad。CB已修补f954旧input callback缺native receipt的P2代码缺口；当前a4干净revision和跨包/前端/脚本/真实MSVC ABI软件结果均已交付，不再描述为merge进行中或补丁待追加。CB六项Swift Recording仍未编译/执行，完整Mac输入退役、真实输入/拓扑不能据Rust和静态合同整体关闭。

GUI044/045、producer28与包含b111/bf5d及两处token补修的Sd2完整历史已进入候选；规范u64字符串与历史日志归一、原Rust签名请求→本地HTTP验签/schema→SQLite/store/analysis的软件联合验收已执行，含前述19个fresh案例，不再是待owner交付或待本地运行。它不证明真实GUI诊断送达、生产服务/网络、设备或relay恢复；也不按旧joint schema永久过滤事件。GPU235、文件030/协议14与024后续生产接线保持独立后续波次，不合入第一波。失败、忽略与缺设备条件分别保留；Swift/Xcode/Metal、实际WebView、权限/输入注入/双机/拓扑/硬件测量继续单列。软件候选通过后由主会话决定合入main，root未知dirty不操作。

### 历史021前置与候选承接：初始化时保留已有partial

独立只读review及固定媒体89bd1e5核对发现的DL2-021缺口已由媒体 `65c70fd658d0526a8a27b75fdbf120ae065222bf` 修复：旧实现ready发送失败直接return会删除已有resume partial，新实现在同一磁盘线程先调用file.preserve再退出。工程组当时只读核对源码及J最终原日志四项初始化回归ok，未自行复跑J；旧89因果链及“源修复/局部joint已记录、第一波待验”的阶段状态保留历史身份。后续a4已完整聚合并通过Windows软件套件，不再把第一波软件验收写成当前待执行；真实慢盘/设备预算仍待验。

65c提交TODO及协调方来源报告transfer28通过、lib/tests严格Clippy与定向rustfmt/diff通过。新增源码回归使用真实spawn_with/create路径，在已取得独占句柄、读取/哈希前用barrier停住，poll Pending后drop创建future，再放行并等待磁盘线程退出；9次取消均断言原4字节prefix、offset/hash/元数据不变，最终续传通过完整哈希。还覆盖空part保留、显式cancel删除、初始化TooLarge不删旧prefix、Windows真实sync_data失败返回IoFailed且未发布；资源型测试串行化而未放大生产8线程上限。28项计数保留为65c来源，J四项原日志及a4完整Windows任务分别提供后续执行证据，不把来源计数冒称当前单独复跑数。

| 必须取得的实际证据 | 不能用来替代的结果 |
| --- | --- |
| 真实临时文件预置有效prefix/resume元数据；barrier精确停在打开后、ready交付前，drop create future，再释放线程；断言partial内容/长度/hash保留并能再次resume | 只测试ready已交付后的handle-drop、sleep碰运气、源码出现preserve、仅assert worker退出 |
| 正常handle-drop仍保留；明确cancel/校验失败仍按原政策清理，资源与并发permit收尾 | 把断连和显式cancel都改成永久保留，或只证明future返回 |
| 独立修复来源/本地复用SHA、最终joint revision和该窗口对应测试/相关套件原始结果 | 两个源分支各自通过、旧suite通过、没有复现新缺陷的只读review结论 |

上表来源层已有65c修复和28项来源结果，J578原日志有四项初始化回归通过，当前a4第一波又按精确干净源码执行Windows及完整软件门禁。后续成功不改写J634/2/19历史报告，也不关闭真实慢盘、设备预算或021/041剩余平台验收。

此前协调方报告joint未提交、索引UU、测试未跟踪，是早期审查快照；工程组当时没有读取或操作这些WIP，该阶段等待最终证据的状态不能延续为当前结论。后续J578和a4均已固定源码并取得各自报告。协调方旧shared汇总为174通过/1忽略，原274为已撤回笔误；该计数只属于当时来源，不是当前工程执行总量或a4完整结果。初始化缺口的旧源码确认、来源回归与最终候选软件验收分别保留。
