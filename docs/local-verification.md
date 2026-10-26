# 分层本地验证

更新日期：2026-10-26。入口是 `python scripts/verify.py`（Python 3.9+，macOS 通常使用 `python3`）；PowerShell 的 `scripts/verify.ps1` 与 POSIX 的 `scripts/verify.sh` 转发到同一实现。从任意工作目录调用脚本都使用它所在的 checkout，不依赖终端当前目录。

| 层 | 默认任务 | 证明范围 |
| --- | --- | --- |
| `fast`（默认） | 三个 Bun 项目 frozen 安装、类型检查、测试；Python 脚本测试；Rust fmt 与 workspace lib 测试 | 本地领域规则、现有契约和脚本行为；首次 Rust 编译仍可能较慢 |
| `integration` | 前端生产构建、Rust Clippy、protocol/video/session/crypto 契约、localhost transport/relay、direct-probe 的 crypto/transport 检查、FFI runtime、媒体恢复、Windows runtime 契约 | 本地进程/流/ABI 的已有自动覆盖；Windows 专有任务在其他平台 skipped |
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

严格几何绑定的共享契约由 Apple/FFI 单一 owner 交付，仍属未发布协议 13 开发线。已协调的字段顺序是 `VideoConfig` 在 `stream_id` 后增加非零 `input_epoch: u64`；`InputEnvelope` 在 `timestamp_us` 后增加非零 `stream_id: u64`、`input_epoch: u64`。Rust 提供 `InputContext { stream_id, input_epoch }`，发送接口显式接收它；**不得在发送时补当前值**。input codec 统一使用保留前缀 `[0, PROTOCOL_VERSION:u16 little-endian]` 再序列化 envelope，通过 protocol 的 `encode_input` / `decode_session_input` 进入；平台/transport 不手工拼前缀，也不 fallback 到无 context 格式。本工程尚未取得该契约 SHA，未引入这些字段或前缀；以下是待运行验收条件，不是已通过记录。

| 门禁 | 必须从生产边界观察到的结果 | 执行层/证据边界 |
| --- | --- | --- |
| context 非零、前缀与完整传递 | codec/ABI/Swift/TS 真实往返保留 stream/epoch；任一值为零、缺失或错代不得默认补当前上下文；验证 seq/timestamp 有效但 context 过期的输入，并拒绝旧合法 Function(1) 等包误解为新 MouseButton、错误/截断前缀和无 context fallback | protocol/FFI/平台桥的实际调用回归；由 owner 的 codec 合同证明新前缀与旧合法非零 sequence 不相交；字段名正则匹配不提供证明 |
| 排队旧事件与旧 release | 冻结发送器，在旧 epoch 入队后切换；正常输入、key-up/ReleaseAll 均保留原 context 并被 host 拒绝；新 epoch 按下的同一键不会被旧 release 释放 | 发送队列与记录型 host injector；仅本机 topology 代数或丢普通 press、保留所有 release 不满足门禁 |
| 首帧实际呈现前禁用 | 仅收到 VideoConfig、解码完成但 presented 尚未发生都不能启用输入；旧 stream/epoch 的迟到呈现不能启用当前 context；事件在入队时已固定 context | Windows 合成宿主、实际 WebView2 分别记录；Apple 原生 presented handler 必须另有真机回调结果 |
| host 原子清键/推进/映射 | 在受控 injector 中阻塞切换竞争，观察清键、epoch 推进、映射发布和锁内核对形成同一边界；旧坐标不能落到新屏 | Windows host 本地集成与真实切屏分开；不以锁字段存在或 config_version 相同推导正确 |
| 取消独立生效 | 普通输入队列满/发送背压、旧输入待丢弃、新首帧未呈现时，用户取消与 host 本机收回仍可停止/清理；不能先要求有效远端 context 或等待普通队列空位 | 合成背压/取消/迟到回调；实际权限/设备另记 |
| probe 重放窗隔离 | 双向慢 probe 经 256 个视频包后仍能认证；probe 重放和跨 lane 解密仍拒绝；真实 shared endpoint 等待可取消且不持媒体 crypto 锁 | 已复用的独立提交 `0836a8b` 与 shared-probe 前置；`--task direct-probe` 跑 crypto/transport 的 `--lib --tests --locked`，不证明双机/WAN p95/p99 |

前五项必须在真实契约及 host/UI 接线位于**同一 revision** 后执行。当前仅为 probe 开发接入；epoch 合同和两端未全部接通前属于不可交付的过渡状态。当前不会注册一个仅 grep 字段或永远退出 0 的“epoch 检查”冒充覆盖；现有全套任务通过也不关闭这些门禁。DL2-013 严格绑定继续保持 `[ ]`，seq、wallclock、仅 config_version 都不能替代它。

probe 复用链与工程源码快照见[能力/版本记录](current-capabilities.md)：原 lane 0～7 不重排，仅追加 `SecureLane::DirectProbe = 8` 并更新 COUNT/ALL；未改 JOIN 版本或 QUIC 事件槽数量。本分支协议 13 与 main 基线协议 12 分别记录，来源分支的 79 项报告不代替本工程接入测试。双机黑洞、慢候选、半包下输入/画面 p95/p99 和锁持有分布仍须另验。

本工程实际接入报告 `dist/verify/probe-integration.json` 为 passed：crypto/transport 79、workspace lib 259、protocol/video/session/crypto 109、relay 27、FFI runtime 39、媒体恢复 2、Windows 契约 131、Python 134 项通过，fmt 通过；计数有重叠，不能相加为独立覆盖率。`probe-clippy.json` 的 frozen 前端准备/生产构建及 workspace 全 target/feature Clippy 通过。两份报告记录源码 `4c3648a`、dirty=true（增加本任务注册/说明），未运行真实远控、注入、Apple presented 回调或双机性能；没有以来源分支结果冒充本次执行。
