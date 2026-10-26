# 分层本地验证

更新日期：2026-10-26。入口是 `python scripts/verify.py`（Python 3.9+，macOS 通常使用 `python3`）；PowerShell 的 `scripts/verify.ps1` 与 POSIX 的 `scripts/verify.sh` 转发到同一实现。从任意工作目录调用脚本都使用它所在的 checkout，不依赖终端当前目录。

| 层 | 默认任务 | 证明范围 |
| --- | --- | --- |
| `fast`（默认） | 三个 Bun 项目 frozen 安装、类型检查、测试；Python 脚本测试；Rust fmt 与 workspace lib 测试 | 本地领域规则、现有契约和脚本行为；首次 Rust 编译仍可能较慢 |
| `integration` | 前端生产构建、Rust Clippy、protocol/video/session/crypto 契约、localhost transport/relay、FFI runtime、媒体恢复、Windows runtime 契约 | 本地进程/流/ABI 的已有自动覆盖；Windows 专有任务在其他平台 skipped |
| `native` | Apple Silicon FFI build、AppleCore Swift 测试、macOS 应用 Swift 测试；Windows 交互桌面捕获/编码 | 依赖平台、SDK、权限、设备；捕获与可能访问原生输入 API 的任务须显式 `--allow-device` |
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
