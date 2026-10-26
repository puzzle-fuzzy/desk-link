# 全分支整合记录

日期：2026-10-26。将本地及 origin 分支全部纳入同一主分支历史，保留各来源提交。应用版本为 0.1.93，业务协议为 14；本次没有发布安装包或部署线上服务。

## 合并范围

合并前先 fetch 全部远程引用，并核对 17 个 checkout。main 上的未提交 TODO 审查记录先保存为 `cb819aa6e92965ecd466df695a9f571c14d499e4`，其余业务工作区无未提交代码。以下九个独立分支尖端覆盖其余全部来源；整合后逐一检查全部本地及远程分支的祖先关系。

| 来源 | 合并时的提交 |
| --- | --- |
| 第一波及 GPU 完整历史 | `babe316f77a1a8cea0fcc0aab6acf6ca8976d45a` |
| 文件与目录任务 | `a6d88e2fc660c2005e3946103287b28f53e68880` |
| LAN 业务运行时 | `187501e2512cf221c9280c3a71f169062a05a3c0` |
| 公开测试更新通道 | `2dfe3df744a0b1ab0c1ad4044b6904c2c1764855` |
| ChaCha20Poly1305 升级 | `cd44ec572d5c48971dc7383be9d4e624d8ce2624` |
| Ed25519 升级 | `12f51bb2699430814c8e7b5074b138ae6394dcd9` |
| rand_chacha 升级 | `cc9c00bf4175392dff0f2032e8cb7e119817ff8a` |
| rcgen 升级 | `c75c52e55500ae865c9a3f7c9c5dfd80b85e1878` |
| Windows bindings 升级 | `95ae6e940e6b0224fb863ee167b345ec1edc872f` |

公开更新分支的 80 个变更路径（含删除）与已有 main 祖先 `85bdf7278baf11b1adb9b401b978643c261daf2b` 的 tree 条目逐一相等，因此该分支只补充合并祖先关系，不回退后续客户端。其余独立分支正常合并并处理实际冲突。保留全部来源历史，以普通推送更新主分支，未执行删除分支命令。GitHub 自动清理的已合并 Dependabot 远程分支，其提交仍完整保留在 main 历史中。

## 整合适配

- 保留 GPU、Apple 输入上下文、诊断 u64、服务端和工程历史。TODO 原审查中的 DL2-047/048 及历史验证身份保留。
- Windows Host 与控制端的完整文件任务走认证后的 SessionDataPlane Transfer 通道；保留逐项身份、u64 offset、队列与恢复行为。可选文件通道失败或泵超时会回收该 actor，输入和控制通道继续运行。控制端仅为已认证的 Windows Host 启动文件任务服务，其他平台立即显示不支持，避免主动能力消息触发 FFI Host 的协议拒绝；原有输入、控制和剪贴板继续使用。未实现 LAN 的 FFI Host 继续明确拒绝该能力。
- 新增本地 QUIC/Noise 回归：LAN 双向新文件消息、真实目录/多块文件任务、超过 u32 的合成块 offset、Transfer 缺路时实际暂停并保留 Input/Control。临时目录由 fixture 创建和清理，无真实远控输入注入。
- 合并后的锁文件采用 chacha20poly1305 0.11.0、ed25519-dalek 3.0.0、rand_chacha 0.10.0、rcgen 0.14.9，以及直接依赖的 windows/windows-core 0.62.2。Tauri 所需旧版 Windows 间接依赖保留。
- 保留公开 rand_core 0.6 边界和身份生成的熵读取顺序，临时私钥种子使用 Zeroizing；AEAD 明确启用 zeroize 并由 trait 断言约束。原有密钥、Noise、签名测试向量保留，测试 RNG 桥接另有固定 ChaCha20 向量。
- rcgen signing_key 和 Windows 0.62 的 API 改名同步到生产及测试源码；生成的 CodeGraph 数据仅在本机忽略。

## 本次验证

最终 Rust 验证源码为干净提交 `60a9afcb9262b354a9cbc381444e9e1783cd45b5`，tree `d323f1860a24cac3a7caf01d020f1e5401b5305e`。本记录后继只补充此验证结果，源码、依赖与测试保持相同。日志、引用审计及源码身份保存在本地 `E:/desk-link/.worktrees/dl2-targets/all-branches-integration/evidence/`。

| 检查 | 本次结果 |
| --- | --- |
| Rust 全 workspace / all features / lib 与 tests | 58 个测试程序，1011 通过、0 失败、38 按配置忽略；含新增平台门控、LAN 文件任务和实际暂停回归 |
| Rust 全 targets / all features check 与严格 Clippy | 均通过，使用 `--locked`；Clippy 使用 `-D warnings` |
| Rust fmt | 全 workspace 检查通过 |
| Windows UI Bun | 508 通过、0 失败；2052 assertions；类型检查、输入合同检查与生产构建通过 |
| account Bun | 79 通过、0 失败；1072 assertions；类型检查通过 |
| diagnostics Bun | 129 通过、0 失败；6017 assertions；类型检查通过 |
| Python 脚本回归 | 263 通过、0 失败 |
| FFI 双头同步 | `sync-ffi-header.py --check` 通过 |

Bun 使用固定 1.3.14，服务安装使用 frozen lockfile，两个服务锁文件未变化。服务验证的 71 个源码指纹与最终提交逐一相同；前端源码、脚本、配置和锁文件也与实际测试输入一致。Rust 最终命令为 `test --workspace --all-features --lib --tests --no-fail-fast --locked -- --test-threads=1`。all-features 的 installer 使用构建脚本提供的验证占位 payload，没有生成可安装发布包。

旧分支报告继续绑定原提交，不作为此整合源码的测试结果；38 个忽略用例保持其硬件、容器、长时运行或专用 fixture 等显式执行条件。

Swift/Xcode、真实桌面捕获与远控输入、双机与设备矩阵、同画质性能、安装签名和线上部署均未在本轮重新验收。原文件分支超过 4 GiB 的独立回读证据仍只绑定其原提交，本轮不会将其重标为整合主分支的结果。
