# DL2-052：relay 资源边界

修复主题：`fix(DL2-052): bound relay QUIC buffers and validate admission budgets`。

## 核实的旧配置

仓库锁定 Quinn 0.11.11 / quinn-proto 0.11.16。实际依赖源码 `config/transport.rs` 的默认值为 uni/bidi 各 100、单流接收窗口 1,250,000 字节、连接窗口 `VarInt::MAX`、发送窗口 10,000,000 字节、datagram 接收 1,250,000 字节、发送 1,048,576 字节。它们是流控或缓存额度，不是连接时立即分配的 RSS。

旧 relay 只覆盖 keepalive/idle，而业务不消费 uni。100 条未读取 uni 流的载荷额度合计 125,000,000 字节（约 119.2MiB），仍受实际网络传递与 Quinn 实现约束；不能把这个算术变成精确 OOM 倍数。旧 Compose 在 256MiB 容器中配置 512 连接，与上述额度及应用缓存没有共同预算。

## 实现的限制

| 项 | 当前明确限制 |
| --- | --- |
| 远端 uni | 0，协议完全禁用 |
| 远端 bidi | 17（16 个可靠转发任务及 join 余量） |
| 单流接收窗口 | 65549 字节（65536 载荷 + 9 路由 + 4 长度） |
| 连接接收 / 发送窗口 | 各 128KiB |
| datagram 接收 / 发送缓存 | 32 / 64KiB |
| crypto 接收缓存 | 16KiB |
| 待接受 incoming | 64 个、每个最多 16KiB、总共 1MiB |
| 默认 accepted connection / session 上限 | 各 64 |

`RelayConfig.buffer_budget_bytes` 和 `DESKLINK_RELAY_BUFFER_BUDGET_MIB` 声明缓冲预算，默认 160MiB。启动及 `--check-config` 会拒绝超出预算的连接配置。每个 accepted connection 取整建模为 2MiB，覆盖上表 QUIC 额度、16 个最大可靠消息缓冲、019 的三类有界媒体队列及 join 缓冲；拒绝握手任务的 64 个槽另计，incoming 队列也单独计入。默认算式为 `64×2MiB + 64×376832B + 1MiB = 152MiB`，没有漏掉拒绝任务的预算。

Compose 和 Dockerfile 的候选默认同步为 64 连接 /64 session /160MiB 缓冲预算，容器限制仍为 256MiB；没有修改正在运行的服务。预算是载荷模型，余量还要容纳 allocator、碎片/流/包元数据、TLS、task、线程和系统开销，不能当作 152MiB RSS 硬上限。扩大连接数必须重新配置预算和容器、并重新测量，不能只修改预算数字。

Linux 主进程每 60 秒记录固定字段的 `relay_memory`（RSS、cgroup-v2 current/peak/limit/oom_kill）；未知值为 null。`--resource-observation` 提供同样的只读 JSON，供本地容器夹具在 scratch image 内读取，不依赖 shell/cat。观察进程本身也计入 cgroup，不能把 cgroup 总量称为 server-only RSS。

## 已运行的本地回归

仓库工具链、`--locked`，`CARGO_TARGET_DIR=E:\desk-link\.worktrees\dl2-targets\network`：relay **38 项常规测试通过**（lib 6、bin 1、resource 3、session 28），另有此前手工性能 probe 默认忽略；all-targets Clippy `-D warnings`、fmt 和 diff 检查通过。

新增真实 QUIC 夹具验证 uni 无 credit、17 条 bidi 之外的打开等待；这不是模拟发送违规 STREAM 包，更不能把 API 等待说成服务器主动关闭恶意连接。完整 65536 字节可靠消息能跨窗口发送；停读接收端确实阻塞后，其他会话 Input 仍可达，关闭后连接容量回收。没有公网流量或真实输入注入。

本条目前只完成代码和本地自动回归。容器峰值、OOM 事件和更大负载下的资源数据待独立本地容器夹具测量；未认证会话的 deadline 属于 DL2-053，不能用 transport idle timeout 代替应用层 admission 清理。
