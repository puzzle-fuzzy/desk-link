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

## 接口计划与协议所有权

1. 由 transport 统一持有认证 Noise 的业务 seal/open、逐通道收发、真实 QuicClient relay backend 和同密文回退；一个 lane 只允许一个 reader。Host/Controller 使用已解密的业务正文，继续调用原正式 codec，不读取或重贴 InputContext。
2. 现有 DeviceCapabilities 只有平台、角色和视频字段，不能声明 DLD1/DLB1 支持。协议 14 由文件任务唯一维护；已经向其协调在初次认证能力交换表达信封模式、逐 lane 集合与明确切换边界。未取得双方能力交集时保持原 relay/视频模式，不能仅凭 probe 成功启用可靠 LAN。
3. 优先在同一 Noise 会话第一条业务前确定 payload mode：relay 与 LAN 共用相同加密正文，后续只切路由，避免旧 relay backlog 与新包装混流。DirectLanConnection 仍须来自已认证 probe，绑定候选及会话；切换保留每 lane 顺序和同密文去重。
4. Host 输入、控制、音频、文件分别消费独立有界 lane；慢文件/磁盘沿用已有 worker，媒体沿用现有 packetizer/队列。只调整实际数据路径与信封预算，不改变 GPU、文件授权/落盘语义或输入格式。
5. 已认证 LAN 会话的 relay 离线策略必须明确：不重新授权、不接收新的身份材料、不把 relay 错误直接冒充 LAN 失效；本地结束/撤权应走仍存活的认证控制通道。LAN 与 relay 都不可用或输入发送结果不确定时，遵守既有 ReleaseAll/会话收尾。

真实 Windows Host/Controller、双向切换/部分降级、断公网存活及任务清理仍需后续实现与自动验证。发现/本地配对、双机延迟/长稳与公网 NAT 环境保留未完成。

## 真实 relay backend 接口切片

`QuicClient::send_channel_for_generation` 和 `next_channel_for_generation` 对原有八个 ChannelKind 提供一致入口，原密文不重新 seal，仍使用既有可靠通道锁与 generation 检查；控制读取复用 pending control 保护，不消费新 peer 的握手。零/错误 generation 不会发送或读取业务。`peer_generation`、`is_closed`、`closed` 是只读健康观察，后者不偷走 runtime 的 Closed 事件。

新增实际 localhost QUIC 行为验证逐 lane 字节一致和错误 generation 拒绝，以及健康等待后原 Closed 事件仍可消费。使用同一独立 target，`test -p desklink-transport --lib --tests --locked --quiet` 为 45 单元+23 localhost 通过，`clippy -p desklink-transport --all-targets --locked -- -D warnings` 与包级 fmt 通过。这是业务适配基础接口，未把它描述成可靠 LAN 生产已启用。

## 完整 Apple 稳定来源整合

在上述历史之上以双父 merge 引入完整 `f954a68153168a1ded048ddf93eb9c565684d096`，不挑旧 ABI 半成品。保留 C/worker 捕获的 InputContext、Host scoped producer generation、1 秒控制发送预算与 owned input future；Controller 保留 Windows 的 `next_event_with_transfer(false)` 隔离慢文件能力并贯穿 Apple 绝对静默时钟分支。Windows 两处冲突保持 joint 的真实 epoch/配置/拓扑；没有恢复 epoch 0。DirectLanConnection 在两个成功认证 probe 边界记录已验证的 session binding，供下一步 attach 核对，不改 probe wire。

资源冲突采用有类型的 `RelayServer::bind_with_flow_control_limits`：只允许低于 DL2-052 上限的窗口，其他 uni/bidi/datagram/crypto/incoming 预算仍由 relay 设置。不能直接把 Apple 的任意 TransportConfig 入口放在预算配置之后，也不能覆盖其真实 2KiB flight 背压设置后宣称夹具通过。新增真实构造回归分别拒绝 send/connection/stream 超上限 1 字节；FFI 原背压夹具改用此入口。

同一独立 target 实际执行六包 `test --lib --tests --locked --quiet`：**592 passed / 0 failed / 21 ignored**（FFI 全 88；Windows 283；其余四包 221）。C ABI 用本机已安装 MSVC 14.44.35207 与 SDK 10.0.26100.0 的 include 环境运行，未安装或更换工具链。初轮暴露两个 private test 参数冲突，已修为显式 include_transfer=true；第二轮因非开发终端 PATH 缺 cl 失败，补本次命令局部 PATH/INCLUDE 后完整通过，未跳过 C 布局测试。正式通过日志 `E:\desk-link\.worktrees\dl2-targets\lan-runtime-merge-apple-msvc.log`，失败轮分别保留在 `lan-runtime-merge-apple.log`、`lan-runtime-merge-apple-final.log`。六包 all-targets Clippy `--locked -- -D warnings` 通过，格式与 diff 检查通过。

这些是 Windows 上的真实本地加密/生产 helper 与 ABI/合成回归；未执行 Swift 编译、macOS 原生捕获、实际键盘注入或双机验收。协议 14 与 LAN 生产能力仍将在唯一来源基础上单独接入，不把 13 整合当 LAN 闭环。
