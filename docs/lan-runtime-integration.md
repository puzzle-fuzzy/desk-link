# DL2-024：生产 LAN 数据面接线

## 冻结来源与整合

第一波网络分支保持 `4b15561586fab7a1481ba6dd4ff4854f34123018` 不动。本工作树 `codex/dl2-lan-runtime` 从该提交建立，使用双父 merge 整合冻结 Windows joint `5782341e22ba55b2efe30a8eeaeafbebfe0daaba`，不重写来源历史、不修改主检出或冻结分支。

七个冲突逐块处理：Windows runtime、runtime_contract、FFI controller、controller_runtime 测试和输入限制记录使用 joint 的完整生产实现；其显式 epoch、输入 owned future、配置队列、DirectLan 取消/仲裁已包含第一波依赖，不能把旧零上下文占位套回。DirectProbe 保留 joint 的 TLS/endpoint 终止回归，并保留网络切片唯一额外的 crate 内认证连接 getter。TODO 保留 joint 的 009 进度与网络 049/052/053/057 条目，两个独立条目 049/050 同时保留。

2026-10-26 在本工作树、独立 `E:\desk-link\.worktrees\dl2-targets\lan-runtime` target，使用仓库 Rust 1.97.1 与 `scripts/run-windows-cargo.py` 实际执行：

```text
test -p desklink-protocol -p desklink-crypto -p desklink-transport -p desklink-relay --lib --tests --locked --quiet
# 215 passed / 0 failed / 4 ignored
test -p desklink-windows --lib --tests --locked --quiet
# 283 passed / 0 failed / 17 ignored
```

原始日志是 `E:\desk-link\.worktrees\dl2-targets\lan-runtime-merge-shared.log` 与 `lan-runtime-merge-windows.log`。ignored 保持原有桌面/硬件/线上/手工性能验收条件，未计作通过。上述只证明首次来源整合，尚不证明 LAN 已生产接线；当时 Apple 的完整上下文来源尚未引入，后续完整整合见下节。

## 首次整合时的接口计划与协议所有权（历史）

1. 由 transport 统一持有认证 Noise 的业务 seal/open、逐通道收发、真实 QuicClient relay backend 和同密文回退；一个 lane 只允许一个 reader。Host/Controller 使用已解密的业务正文，继续调用原正式 codec，不读取或重贴 InputContext。
2. 当时 DeviceCapabilities 只有平台、角色和视频字段，不能声明 DLD1/DLB1 支持。协议 14 由文件任务唯一维护；当时向其协调在初次认证能力交换表达信封模式、逐 lane 集合与明确切换边界，后续已复用其唯一来源。未取得双方能力交集时保持原 relay/视频模式，不能仅凭 probe 成功启用可靠 LAN。
3. 优先在同一 Noise 会话第一条业务前确定 payload mode：relay 与 LAN 共用相同加密正文，后续只切路由，避免旧 relay backlog 与新包装混流。DirectLanConnection 仍须来自已认证 probe，绑定候选及会话；切换保留每 lane 顺序和同密文去重。
4. Host 输入、控制、音频、文件分别消费独立有界 lane；慢文件/磁盘沿用已有 worker，媒体沿用现有 packetizer/队列。只调整实际数据路径与信封预算，不改变 GPU、文件授权/落盘语义或输入格式。
5. 已认证 LAN 会话的 relay 离线策略必须明确：不重新授权、不接收新的身份材料、不把 relay 错误直接冒充 LAN 失效；本地结束/撤权应走仍存活的认证控制通道。LAN 与 relay 都不可用或输入发送结果不确定时，遵守既有 ReleaseAll/会话收尾。

上述首次整合时，真实 Windows Host/Controller、双向切换/部分降级、断公网存活及任务清理尚待实现；当前已验证实现见后文生产接线段。发现/本地配对、双机延迟/长稳与公网 NAT 环境仍保留未完成。

## 真实 relay backend 接口切片

`QuicClient::send_channel_for_generation` 和 `next_channel_for_generation` 对原有八个 ChannelKind 提供一致入口，原密文不重新 seal，仍使用既有可靠通道锁与 generation 检查；控制读取复用 pending control 保护，不消费新 peer 的握手。零/错误 generation 不会发送或读取业务。`peer_generation`、`is_closed`、`closed` 是只读健康观察，后者不偷走 runtime 的 Closed 事件。

新增实际 localhost QUIC 行为验证逐 lane 字节一致和错误 generation 拒绝，以及健康等待后原 Closed 事件仍可消费。使用同一独立 target，`test -p desklink-transport --lib --tests --locked --quiet` 为 45 单元+23 localhost 通过，`clippy -p desklink-transport --all-targets --locked -- -D warnings` 与包级 fmt 通过。这是业务适配基础接口，未把它描述成可靠 LAN 生产已启用。

## 完整 Apple 稳定来源整合

在上述历史之上以双父 merge 引入完整 `f954a68153168a1ded048ddf93eb9c565684d096`，不挑旧 ABI 半成品。保留 C/worker 捕获的 InputContext、Host scoped producer generation、1 秒控制发送预算与 owned input future；Controller 保留 Windows 的 `next_event_with_transfer(false)` 隔离慢文件能力并贯穿 Apple 绝对静默时钟分支。Windows 两处冲突保持 joint 的真实 epoch/配置/拓扑；没有恢复 epoch 0。DirectLanConnection 在两个成功认证 probe 边界记录已验证的 session binding，供下一步 attach 核对，不改 probe wire。

资源冲突采用有类型的 `RelayServer::bind_with_flow_control_limits`：只允许低于 DL2-052 上限的窗口，其他 uni/bidi/datagram/crypto/incoming 预算仍由 relay 设置。不能直接把 Apple 的任意 TransportConfig 入口放在预算配置之后，也不能覆盖其真实 2KiB flight 背压设置后宣称夹具通过。新增真实构造回归分别拒绝 send/connection/stream 超上限 1 字节；FFI 原背压夹具改用此入口。

同一独立 target 实际执行六包 `test --lib --tests --locked --quiet`：**592 passed / 0 failed / 21 ignored**（FFI 全 88；Windows 283；其余四包 221）。C ABI 用本机已安装 MSVC 14.44.35207 与 SDK 10.0.26100.0 的 include 环境运行，未安装或更换工具链。初轮暴露两个 private test 参数冲突，已修为显式 include_transfer=true；第二轮因非开发终端 PATH 缺 cl 失败，补本次命令局部 PATH/INCLUDE 后完整通过，未跳过 C 布局测试。正式通过日志 `E:\desk-link\.worktrees\dl2-targets\lan-runtime-merge-apple-msvc.log`，失败轮分别保留在 `lan-runtime-merge-apple.log`、`lan-runtime-merge-apple-final.log`。六包 all-targets Clippy `--locked -- -D warnings` 通过，格式与 diff 检查通过。

这些是 Windows 上的真实本地加密/生产 helper 与 ABI/合成回归；未执行 Swift 编译、macOS 原生捕获、实际键盘注入或双机验收。协议 14 与 LAN 生产能力仍将在唯一来源基础上单独接入，不把 13 整合当 LAN 闭环。

## Windows Host / Controller 生产接线

该节对应第二波最终生产切片，不再是 opt-in 传输夹具。业务14来源通过 `-x` 保留：`c919ecc33e89fc17fa9a5f32232628f3aa280af8` 对应 `5decea41a272ac1bcd42590b97a66fe02cc5e3e6`，`5c4861a2ac021d7c94b2d39855c033def8a2f184` 对应 `4d8ea4025a81c3797df87af94276daf35a9724c4`。Root 未另起业务版本；输入格式、Noise 和 JOIN 不变。两份 C 头与 Swift `WireProtocolVersion` 由既有 `sync-ffi-header.py` 从同一 Rust 常量生成14；Cursor 的三份 Swift bytes 夹具同步到14，并由真实 Rust codec 对照。新文件合同及测试仅经过同工具链 rustfmt，不改变来源定义或文件业务语义。

Windows Host 和 Windows Controller 仅在实际支持的能力里声明 `Some(all8, continuation=true)`，真实双端交集确定逐 lane 模式。Mac/iOS Controller、FFI Mac Host 保持 `None`，未经协商的 LanDataSwitch 明确拒绝；FFI Host 与 Windows 业务层对尚无服务的 FileTask 明确报 unsupported，通用 ControllerRuntime 只把它解码透传为 Transfer 事件，不代表服务可用。不能从一个传输能力位推断新大文件/目录服务已经接通。Root 只接现有文件业务，DL2-030 的新服务仍归原 owner。

Host 在 Noise/授权及初次 Hello/Capabilities 后建立数据面，Capabilities 是最后一条原格式 bootstrap；DisplayList、quality 和其余生产业务从第一条就交给统一 plane。候选 TTL 从捕获准备/初次能力完成之后才开始，避免慢设备启动耗尽候选有效期；Controller 对过期候选/已退休 probe 的迟到 Offer 保持 relay，不发送无认证连接的 Ready。错误 binding/乱序 switch 仍拒绝。

Host 的 Input、Control、Audio、Cursor、VideoConfig、视频/关键帧与 Transfer 均使用同一 plane 的明文接口，各 lane 独立消费；仍复用原 packetizer、配置队列、GPU/encoder、磁盘 worker 和权限决策。认证 probe future 在 Control select 外持久 pin，只在完成后重建，持续反馈不会取消半途 probe。Controller 的 Runtime/InputSender/ControlSender/TransferSender 共用同一 plane，保留原 input 专用序号锁、完整 owned future 与 Control 一秒总预算；接收和待配置视频缓存显式区分 Ciphertext/Plaintext，已由 plane 解密的正文不会再次 open。

InputContext 只取调用方原先捕获的 stream/epoch，数据面不采纳新配置重贴。已开始输入的错误/取消会关闭原 plane 和 client，之后旧克隆 sender 拒绝，Host 既有 ExitGuard 负责清键。Host/Controller 的可选 Transfer NoPath/Interrupted 仅取消当前文件/剪贴板操作、停止 worker、暂停队列并拒绝同会话新传输；不重放写入，Input/Control/媒体继续。正在执行的 Input/Paste future 不因文件降级被截断。认证/codec/核心通道错误仍走原终止/重新认证策略。

已认证 LAN 的延续只保留该 Noise 会话和冻结能力，不接受新配对、发现或新的身份材料。双方同意且 Control/Input 已通过远端屏障时，relay 离线继续既有 LAN；可选通道分别报告不可用。重新启用失败路径需要新的已确认切换/会话边界，没有静默换代。LAN-only 本地结束没有新增业务消费 ACK，按既有策略保留 **unconfirmed / 安全暂停**，不把旧 peer_disconnected flag、WAN 中断或更换 peer 当成本次确认。

## 实际生产路径夹具

`apps/windows/src/runtime/lan_tests.rs` 从真实 RelayServer / QUIC / Noise / 两端真实能力开始，运行生产 `receive_host_control`、owned probe、HostLanSwitch 和 ControllerRuntime；持续发送真实 Control 反馈，并确认双方八 lane Direct 后，只关闭本夹具独占 relay。

- 原/旧 InputContext 经正式 codec 与 Host policy/RecordingBackend 验证；无 OS 输入注入。视频使用合成编码字节，音频使用真实 Opus，未声称实际屏幕呈现、WASAPI 播放或硬件延迟。
- 每方向文件 **344087 bytes = 7×49152+23**，完整 BLAKE2s-256 为 `5160248f68622f8b77f56c06b0baccd35017a0ba40b804432910270b130ed53e`。复用生产 OutgoingFileReader/stream_host_outgoing_file 与 IncomingFileWorker，完成真实临时目录写入、size/hash/flush/publish 和完整字节读回；完成 ACK 在 worker.finish 后发送，绝非 LAN admission 即完成。新增入口仅 `#[cfg(test)]` 显式临时根目录/hook，不改变 USERPROFILE、真实 Downloads 或生产磁盘语义。
- 慢写 hook 阻塞首个实际 write，1 chunk 写中、4 chunk 填满 Transfer ingress；sender 完成计数保持5时，在同一 **120ms 回归预算**内验证新 captured MouseMove 和新 timestamp Opus 到达，再释放 gate 完成双向文件。该预算不是实机延迟分位数。
- 取消回归使用正式 Host input future/ExitGuard：先注入 keyDown，再停止 poll 同一 future，32 条 motion 填满队列，20ms 触发取消已 poll 的 owned keyUp。没有手动 close 冒充取消；实际观察 plane/直连关闭、两个 sender 克隆拒绝、Host 只记录 Down→Up 清键，queued motion/取消帧未注入。

首轮夹具提前释放 DirectLanSession endpoint 而超时，修正其所有权后通过；后来改为真实 Host Control loop，并修复其每轮重建 accept future 的取消问题。外层20s是包含实际磁盘的夹具截止预算，不当作耗时/性能结果，120ms隔离预算未放宽。失败/中间日志保留，最终验证使用最新源码。

## 最终本地门禁与边界

同一独立 target、Rust 1.97.1、`scripts/run-windows-cargo.py`、`--locked`：

```text
test -p desklink-protocol -p desklink-crypto -p desklink-transport -p desklink-relay -p desklink-ffi -p desklink-windows -p desklink-windows-ui --lib --tests --locked --quiet
clippy -p desklink-protocol -p desklink-crypto -p desklink-transport -p desklink-relay -p desklink-ffi -p desklink-windows -p desklink-windows-ui --all-targets --locked -- -D warnings
fmt -p desklink-protocol -p desklink-crypto -p desklink-transport -p desklink-relay -p desklink-ffi -p desklink-windows -p desklink-windows-ui -- --check
python scripts/sync-ffi-header.py --check
python -m unittest discover -s scripts/tests -p test_*.py
```

七包 **762 passed / 0 failed / 23 ignored**：protocol63、crypto36、transport72、relay73、FFI93、Windows286、UI139。另显式17秒专项1 passed；其余 ignored 条件未运行且不计通过。Python **207 passed**，双头/生成版本、Clippy all-targets `-D warnings`、fmt/diff 检查通过。C ABI 实际使用已安装 MSVC/SDK；环境与来源见前文。Python 校验同步新 pin/bounded-flow API，production queue scanner 只排除精确 cfg(test) 条件区域/真实 test-only 模块；新增六个夹具确保生产邻居、未知 cfg、只有 `_tests` 文件名的生产代码仍拒绝 unbounded channel，不靠放行测试掩盖真实生产队列。

正式日志位于 `E:\desk-link\.worktrees\dl2-targets\`：`lan-runtime-full-final-04.log`、`lan-runtime-all-clippy-final-03.log`、`lan-runtime-python-final-02.log`；更早各轮仍保留失败和中间证据。资源/保活/背压详细日志见会话 transport 记录，完整来源与文件指纹在最终交接附录绑定。

未运行/未交付：Swift6/macOS/iOS 原生编译和设备 LAN、真正双机捕获/呈现/注入/音频、NAT/UDP受限网络、长稳/吞吐/分位延迟、LAN发现/本地配对。Mac/iOS LAN 能力未启用。未构建新安装包、推送、部署或发布；DL2-024 主项仍保留未完成。DL2-019 的帧依赖丢弃及真实性能仍独立未验，不在 relay 解密或伪称密文可识别帧。
