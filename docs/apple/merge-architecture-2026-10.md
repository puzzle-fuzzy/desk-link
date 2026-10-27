# 2026-10 大合并架构决策记录

> 记录 2026-10-27 将 `codex/macos-parity-phase1-2`（macOS 对齐第 1、2 阶段）合入 main（`23ba00d`）时做出的结构性决策。合并双方对同一批功能各有一套实现，本文记录统一后的形态与理由，供后续开发对照。行号会漂移，以符号名为准。

## 1. 回调事件枚举（ABI）

- 合并双方都在 `DesklinkEventKind` 的 13 号位新增了事件。统一值域（`crates/desklink-ffi/src/lib.rs`）：
  `Capabilities=13`（main），`Session=14`、`Clipboard=15`、`AudioPcm=16`、`File=17`（分支，整体顺延 +1）。
- 两份 C 头（`crates/desklink-ffi/include/desklink.h` 与 `apps/apple/Sources/DeskLinkC/include/desklink.h`）必须字节一致，`tests/header_sync.rs` 门禁校验 Rust/C 枚举值同步，并把新增 ABI 枚举（如 `DesklinkControllerCommand`）纳入同步表。

## 2. 控制端 worker：两套机制叠加而非二选一

main 的传输底座保留为唯一权威，分支的会话语义叠加其上：

- **有序发送**（main）：`ConnectedSenders`（input/control/transfer 三条可克隆 lane）+ `OwnedConnectedSend` 单飞行发送 + `send_cancellable` 会话级取消/超时。事件读取用栈固定 future（"已出队未解密的可靠消息不可丢"）。
- **会话门控**（分支）：`SessionGate{epoch, stream_id, input_epoch}` + `CommandScope`；`ControllerCommand::Session{scope, action}` 携带 `SessionAction`（文本、选屏、画质、音频、剪贴板、文件）。
- **SessionIo 适配层**：`worker.rs` 中的 sender-only I/O。会话命令不能直接借用 `&ControllerRuntime`（被栈固定的 `next_event` 独占借用），`SessionCommands::execute` 通过 `SessionIo` 走三条 lane，保持与 owned-send 相同的有序纪律；测试用 `SessionIo::for_runtime` 构造。
- **文件传输**：`file_commands::FileTransfers` 由 VideoConfig 事件按流重建，独立 `file_gate` 门控；宿主不支持时 C ABI 有界回复 Unsupported（不中断控制会话）。
- **维护定时器**：内层 select 增加 `maintenance.tick()` 分支驱动剪贴板/选屏/音画的 10 秒响应超时。

## 3. 双输入路径（显式上下文 vs 会话派生上下文）

- **显式上下文**（main）：`CapturedInputContext{streamID, inputEpoch, configVersion, runtimeGeneration}`，Swift 在帧真正呈现后确认（`confirmVideoFramePresentation`），`desklink_send_input` 携带完整上下文；`desklink_release_all_for_context` 只释放该表面的按键。
- **会话派生**（分支）：`desklink_send_input_for_stream(expected_stream_id, input)`，Rust 侧从 `SessionGate.input_epoch`（由认证 VideoConfig 写入）派生几何上下文，再走与显式路径完全相同的 `dispatch_input`/按键追踪/释放语义。Swift 的 `send(input:)` 优先捕获呈现上下文，无呈现表面时落到此路径。
- 结论：两条路径共享同一套校验与追踪，仅上下文来源不同；`KeyboardInputState`（policy 物理按键，`KeyboardMappingPolicy`）与视图级 IME 文本（`sendText` → `desklink_send_text` 有界文本队列）分别落在各自合适的入口。

## 4. 统一重连语义（Swift `ControllerBridge`）

- **可恢复错误**（Rust `recoverable=1`）：保留 runtime 与呈现表面，仅置 `.reconnecting`；不做输入退休——Rust 侧 worker 的重连边界已经释放输入。
- **显式重连边界**（STATE `Reconnecting`/`Disconnecting`）：若呈现表面仍被调度器接受（同代同纪元）则原样保留会话；否则 `retireActiveStream()`——释放按键、置 `awaitingReconnectHandshake`、重置解码。
- **握手门**：`Connected` 事件在 `awaitingReconnectHandshake` 期间只接受两种证明：SECURE_HANDSHAKE 状态事件，或调度器当前上下文与事件流一致（复用流 ID 的场景）。仅小于等于高水位的旧流 ID 不可复活。
- 分支 UX 语义保留：重连期间会话视图与缩放状态不销毁；`SECURE_HANDSHAKE → NEGOTIATING → STARTING_VIDEO` 阶段保持 `.reconnecting` 视图。

## 5. 固定密码派生（`a4d80d0`）

- 目录码固定 8 字节（协议/中继不变），用户密码放开为任意 8..=64 UTF-8 字节：`PairingCode::derive_from_password` = 域分离 BLAKE2b-64(`desklink-directory-access-v1` ‖ password)。
- 输入路径：`desklink_host_start_directory` 与 `desklink_connect_directory` 接收密码字节并在 Rust 内派生；`connect_secure`（保存材料）路径中的 `directory_access_code` 是**已派生终码**，原样使用不再二次哈希。
- 传输层 `valid_directory_access_code` 放开为仅禁全零；32 字符字母表只用于生成临时密码的显示。
- 密码逐字节敏感（仅去首尾空白）；迁移日之前保存的旧固定密码记录需重新输入一次。

## 6. Apple 侧双解码器与主机捕获

- **解码调度器**（main）：`H264DecodeScheduler` 在 FFI 回调线程完成媒体准入（代/流/纪元/版本），输出 `H264DecodedFrame` → `consumeDecodedFrame` 建立呈现上下文；输入许可以真实呈现为准。
- **呈现解码器**（分支）：`H264Decoder`（包装 `H264DecodeSession`）服务选屏 UX（`displayConfigurationCache`、`minimumDisplayConfigVersion` 门槛、`finishDisplaySwitchIfReady`）与 `videoConfigVersionForTesting`。两者职责不同，故意并存。
- **HostBridge**：main 的捕获准入（`HostCaptureAdmission`、媒体代、`*_for_generation` 发送）为权威；分支的目录共享（固定/临时密码、`HostDirectoryRuntime`）叠加其上，事件仍经 main 的准入调度管道。

## 7. 部署拓扑备忘（2026-10-28）

- 中继：腾讯云 `101.35.246.159` UDP 4433（Docker，`/opt/desklink-relay`），TLS 复用 `p2p.yxswy.com` 证书线（双 SAN，含 `turn.p2p.yxswy.com`），续期钩子 `/etc/letsencrypt/renewal-hooks/deploy/desklink-relay-tls` 校验主机名并原子切换+重启容器。
- 客户端默认（迁移前）：`quic://turn.p2p.yxswy.com:4433`。单域名收敛方案见 TODO.md「MVP 后产品优化」MVP-02，阶段 4（删 turn.p2p DNS + 证书收缩）须待全部设备重新配对后执行。
- ACME：nginx `/etc/nginx/conf.d/p2p.yxswy.com.conf` 只服务 `/.well-known/acme-challenge/`（webroot `/var/www/p2p-acme`），其余 404；certbot 非交互续期有最长 8 分钟随机延迟，演练用 `certonly --dry-run`。
