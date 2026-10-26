# Windows 联合验证（2026-10-26）

Windows 产品与媒体实现在独立 worktree `codex/dl2-windows-joint` 中正常合并，保留双方原始来源提交。自动验证建立了 Windows 配置、输入上下文与主机映射的本地加密链路；主 DL2-013 保持 `[ ]`，整体软件尚不能据此认定可交付。

| 来源 | 固定提交 | 范围 |
| --- | --- | --- |
| 产品实现 | `16594c4f8780cd565afd57695f88e29277ec5939` | 原逐项提交、显式 InputSender、完整 u64 IPC、绘制后输入门禁 |
| 独立 013 跟进／合并第一 parent | `27fec756ba97ee6f67228dc9530c7b0578a49ff3` | 等价 future 类型别名，修复严格 Clippy；继承上述产品来源 |
| 媒体基线 | `89bd1e5161653dd668d290a417bd5f6cc146290a` | 真实编码器版本与随机非零 epoch 配对、原帧上下文归属 |
| 后继 046 | `165fcca2e74ebc8cc4c60defbc1e04fe0971fdce` | 保留活动直连、只退休匹配连接 |
| 后继 044 | `a599246535b73f57e0425cbbfd18c7156e050bba` | 不支持 cursor datagram 时降级、typed observer；GUI DiagnosticLog 未接入 |
| 后继 045 | `6dbe53d64c60bbdd50af4b592ebef7471b800fcc` | 监听终态退休、禁用迟到 probe 安装 |
| 独立 021 跟进／合并第二 parent | `65c70fd658d0526a8a27b75fdbf120ae065222bf` | 初始化等待取消后保留断点文件，真实取消回归与 worker 测试限流 |

冲突解析同时保留 InputState 独立 watch、实际 apply 后的权限反馈、EndControl 与原 peer 断开确认，以及新增媒体诊断、直连仲裁、配置队列和帧身份。旧 context、暂停或关闭状态下丢弃的输入不会误报权限恢复。新增联合测试文件已纳入版本控制。

| 最终候选测试范围 | 通过 | 失败 | 忽略 |
| --- | ---: | ---: | ---: |
| Windows lib 与 integration tests | 283 | 0 | 17 |
| Windows 管理库 | 135 | 0 | 1 |
| protocol / video / session / crypto / transport | 178 | 0 | 1 |
| FFI lib 与 integration tests | 38 | 2 | 0 |
| Rust 总计 | 634 | 2 | 19 |

Windows lib 单独为 218 通过、2 忽略；两项新增联合回环均包含在该计数内。FFI lib 12 项、controller_runtime 6 项通过。完整 Rust 命令因两项 FFI Mac Host 测试失败而以 Cargo 101 结束（包装脚本 exit 1），不能记成全包通过。相同八包的 `--lib --tests --locked -- -D warnings` 严格 Clippy 全部通过，未使用 `allow` 或 `--no-deps`。Bun **1.3.14**：474 通过、0 失败，71 文件、1894 断言；生成合同检查、tsc 与 Vite 构建通过。

`joint_software_video_config_geometry_and_input_share_the_published_epoch` 实际执行合成 64×64 BGRA → Microsoft SoftwareOnly MFT 编码 → `encoder.config_version()` → Coordinator 发布 → HostVideoPipeline 配置和 IDR → localhost relay QUIC/Noise → 生产 ControllerRuntime 配置消费与组帧 → 捕获 context 的生产输入 sender → 加密 QUIC → HostInboundPolicy／Selected→Input 锁内 guard → RecordingBackend。纯编码变化保持 epoch 和 held 键；同尺寸、同 SPS 换屏通过真实编码器重建推进版本和 epoch；旧 release、pointer 不进入新映射，新按键保持到实际 pressed-state cleanup。没有手改 frame 版本或固定 epoch 兜底。

`joint_new_noise_same_stream_rejects_old_plaintext_and_old_releases` 使用相同身份、完整 u64 stream 和共享 epoch allocator，新建 QUIC/Noise 会话；两个 context 均来自实际 Pipeline 配置经生产 Controller 消费后的结果。生产 sender 在新会话发送旧 context，Host 拒绝且不污染排序；当前 context 仍可输入，旧 release 不清除新按键，旧 ciphertext 不能跨 Noise 使用。另以原样旧 plaintext 在额外新 Noise 会话中重加密后送入 guard，证明精确旧明文仍被拒绝；**这一额外检查为 packet 级，没有经过 QUIC**。

完整重跑还包含 021 实际初始化取消／断点保留、慢盘线程隔离与独占收尾，013 pending 输入期间继续消费新配置，009 真实 apply 反馈，以及 044/045/046 的 cursor 降级、真实直连仲裁和监听关闭回归。

未接入完整 Apple ABI／Mac Host 的两项已知失败为 `desklink-ffi --test host_runtime` 中 `approval_allows_video_and_reject_emits_release_all` 与 `host_decodes_input_and_keyframe_requests_after_approval`。当前 FFI `host_worker.rs` 视频配置仍写零 input_epoch；本次不以常量、跳过或修改断言掩盖该缺项。

19 项 ignored 分别涉及交互桌面捕获、双机／硬件 soak、真实管理服务、签名更新资产，以及显式 release 性能测量，不计入通过。联合回环没有启动屏幕捕获、真实系统输入注入、剪贴板或实际解码呈现。Bun 的 DOM/canvas/rAF 门禁是受控自动证据；真实 Tauri WebView 呈现、物理输入、硬件 MFT、热插拔／旋转／DPI／睡眠、双机体验及 GUI DiagnosticLog 接线仍待验收。

最终运行前后，477 个源码／配置文件的 SHA256 清单均未变化，聚合值为 `b2c38409aef18d34a8d88f9e02186e86635a410dacd322b8c7de25ed3d23094e`。最终提交、tree、父提交、逐文件摘要与日志摘要保存在 [handoff-final.json](E:/desk-link/.worktrees/dl2-targets/windows-joint/handoff-final.json)。本页等证据文档在测试后补充；可执行源码未变化。

最终日志：[Rust 全量](E:/desk-link/.worktrees/dl2-targets/windows-joint/final-applicable-tests.log)、[脚本计数](E:/desk-link/.worktrees/dl2-targets/windows-joint/final-test-counts.json)、[严格 Clippy](E:/desk-link/.worktrees/dl2-targets/windows-joint/final-clippy.log)、[Bun](E:/desk-link/.worktrees/dl2-targets/windows-joint/final-bun-tests.log)、[前端构建](E:/desk-link/.worktrees/dl2-targets/windows-joint/final-bun-build.log)。`pre021-*` 为修复前阶段日志，不作为最终证明。

复跑使用仓库固定 Rust 1.97.1，通过 Windows 工具链包装器，Cargo 串行执行：

```powershell
$env:CARGO_TARGET_DIR = 'E:/desk-link/.worktrees/dl2-targets/windows-joint'
python scripts/run-windows-cargo.py test -p desklink-windows -p desklink-windows-ui -p desklink-ffi -p desklink-protocol -p desklink-video -p desklink-session -p desklink-crypto -p desklink-transport --lib --tests --locked --no-fail-fast
python scripts/run-windows-cargo.py clippy -p desklink-windows -p desklink-windows-ui -p desklink-ffi -p desklink-protocol -p desklink-video -p desklink-session -p desklink-crypto -p desklink-transport --lib --tests --locked -- -D warnings
```

前端在 `apps/windows-ui` 使用 `packageManager` 指定的 Bun 1.3.14，先 `bun install --frozen-lockfile`，再 `bun test`、`bun run build`。
