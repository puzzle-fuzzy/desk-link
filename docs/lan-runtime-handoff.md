# DL2-024：第二波本地交接与证据指纹

本次交付是 Windows Host / Controller 的生产 LAN 业务接线与本地验证。DL2-024 主项仍为未完成；发现/本地配对、Apple 原生 LAN、真实双机、NAT 和性能/长稳验收不在已通过范围。实现及夹具细节见 [生产接线记录](lan-runtime-integration.md) 与 [会话 transport 记录](lan-session-transport.md)。

## 封存来源

工作树：`C:\Users\18267\.codex\worktrees\dl2-lan-runtime\desk-link`，分支：`codex/dl2-lan-runtime`。生产源码验证点为 **`ae42a566ff402efb5658d66adb34f9d2b8821a35`**，其完整 Git tree 为 `66270f4ba4f877de9851281a9f974ac08f978491`。本附录及其链接/日志更新仅修改文档；源码以该验证点为准。

| 切片或来源 | 完整提交 |
| --- | --- |
| 第一波网络冻结，分支未移动 | `4b15561586fab7a1481ba6dd4ff4854f34123018` |
| 冻结 Windows joint 原来源 | `5782341e22ba55b2efe30a8eeaeafbebfe0daaba` |
| Windows / 网络双父整合 | `e7934c1b94005ee55c2215f589a953eb2355adb4` |
| generation 绑定真实 relay backend | `7657a20c7ecec30c274ef1b11c3e810260233061` |
| 完整 Apple 原来源 | `f954a68153168a1ded048ddf93eb9c565684d096` |
| 完整 Apple 双父整合 | `418df74e4b86379dd134932b585ef2776fe5329e` |
| 协议14文件合同导入 | `c919ecc33e89fc17fa9a5f32232628f3aa280af8` |
| 协议14 LAN 协调合同导入 | `5c4861a2ac021d7c94b2d39855c033def8a2f184` |
| 生产会话 transport | `af822f2105f3fd8906d2dd6fdeac6b6aa72b9b4a` |
| Windows / 实际 ControllerRuntime 接线 | `ae42a566ff402efb5658d66adb34f9d2b8821a35` |

上述三个原来源均实际通过 `git merge-base --is-ancestor <source> ae42a566...`。协议14的唯一 owner 原提交分别为 `5decea41a272ac1bcd42590b97a66fe02cc5e3e6`、`4d8ea4025a81c3797df87af94276daf35a9724c4`，导入保留 `-x` 来源。未另起协议版本，未改 Noise/JOIN 或输入合同。

## 实际结果与计数

Rust 1.97.1，所有 Cargo 操作均经仓库 `scripts/run-windows-cargo.py`、`--locked`；独立 target 为 `E:\desk-link\.worktrees\dl2-targets\lan-runtime`。C ABI 测试使用本机既有 MSVC/SDK，具体环境和复现命令见生产接线记录。

- 七包 **762 passed / 0 failed / 23 ignored**，共49个结果组；protocol63、crypto36、transport72、relay73、FFI93、Windows286、UI139。
- Python **207 passed**；七包 all-targets Clippy `-D warnings`、包级 fmt、双 C 头/Swift 版本生成检查通过。
- 生产提交封存后，显式 `--ignored --exact` 再运行17秒静默 LAN 专项：**1 passed，测试阶段17.05秒**。它不计入762，也不把其余 ignored 计作通过。
- business transport 十项常规测试包含主文件8项与 raw-sink 子模块2项；后两项已包含在上述 relay73/总762中，不重复相加。真实7B send、48B stream credit 的 fence 背压回归维持300ms预算。
- 真实 Host / Controller 夹具在双方完成八 lane 屏障后关闭独占 relay，验证原 captured context、真实 Opus、双向各344087B临时文件的完整 hash/flush/publish/完成 ACK、慢文件隔离，以及输入取消后的清键与旧克隆拒绝。它没有执行 OS 输入注入或硬件呈现/播放。

完整门禁在提交前的最终源码上实际执行；提交后重跑静默与轻量源码门禁。以下是本机执行所得原始日志与当前源码的绑定，不能当作 CI、安装器或部署镜像证明。

## 原始日志与清单

下列文件均位于 `E:\desk-link\.worktrees\dl2-targets\`，失败和中间轮日志仍保留。SHA-256 对原始文件字节计算。

| 文件 | 字节 | SHA-256 |
| --- | ---: | --- |
| `lan-runtime-full-final-04.log` | 7139 | `edb6e00f132a03c70cc6cdaa462aa3a768572337189eca8e44da6a81e4085a4f` |
| `lan-runtime-all-clippy-final-03.log` | 427 | `e76e5a4044909129890e4f852e8569831712336145f80b7a7326abb52850fb98` |
| `lan-runtime-python-final-02.log` | 651 | `de377e13f04128f0ab854a98e2284ef8d48a316bea506c0e1740c673a3372076` |
| `lan-runtime-owned-backpressure-final-03.log` | 3431 | `3302c778888645a02431680dcb054f3b66ec44a9c261097084e117fe12351bc6` |
| `lan-runtime-silent-final-02.log` | 1021 | `44eeb5ecb6e4676028e4ee24db5538fb4a1ac08ca0e0bd30e45d753fe693b7d1` |
| `lan-runtime-source-gates-final-01.log` | 134 | `9d4e10c289666f9824032c0ee84525ca0b32c6b47ad850a9cca51c4adb55f1c6` |

机器可读清单：`E:\desk-link\.worktrees\dl2-targets\lan-runtime-ae42a566-evidence.json`，SHA-256 为 **`1e564cc367f0f52b5a38558fc023cdc6b5360fa8d152daa9880abaf65ac8143a`**。它记录上述日志、完整 source/tree/merge/owner 来源，以及18个关键源码/夹具/锁文件的 Git blob 与工作树字节 SHA-256。Git blob 绑定提交内内容，工作树 SHA-256 另保留本机换行后的实际文件字节；二者不混用。

## 仍待完成

Mac/iOS LAN 能力保持 None，Swift/macOS/iOS 原生编译及设备验证未运行。LAN-only 本地结束没有新增业务消费 ACK，仍为 unconfirmed / 安全暂停。强制结束部分写入的可靠帧仍可能触发既有 framing guard 关闭物理 relay，繁忙替换可能需要重新连接/认证。共享 QUIC 连接预算不构成硬带宽预留或真实延迟保证。

发现/本地配对、双机捕获/呈现/注入/音频、NAT/UDP受限网络、长稳/吞吐/分位延迟均保留待验；DL2-019 帧依赖丢弃仍独立未完成，未在 relay 解密或从密文推断帧。未改主检出、构建新安装包、推送、部署或发布。
