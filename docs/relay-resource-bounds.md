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

以上是核心提交阶段的本地自动回归，后续候选容器实测见文末。未认证会话的 deadline 属于 DL2-053，不能用 transport idle timeout 代替应用层 admission 清理。

## Join 前 datagram 的元数据边界

进一步读取锁定依赖的 `connection/datagrams.rs` 确认：接收队列按 `data.len()` 维护 `recv_buffered`。空 datagram 虽进入队列，却不增加这个字节计数，因此只配置 32KiB 接收额度不足以约束其队列元数据。不能将上述载荷预算解释为此类消息数量的硬上限。

relay 现在在整个 join 阶段并行等待首条 datagram，任何 join 前 datagram 都会立即触发应用关闭。正常客户端必须先等 join ACK；已加入会话的 datagram 校验和正常媒体路径不变。join 结果就绪优先处理，避免正常 ACK 后首包被误判；取消未完成 join 时沿用 participant/admission guard，包含登记后回复仍阻塞的生命周期。

本地真实 QUIC 四种回归覆盖空/非空 datagram 与未发 join/仅发部分 join 长度的组合。修复前四项均在 1 秒关闭等待上失败（join 默认 8 秒），修复后全部应用关闭并回收 admission/membership；这是实际红绿回归，不是把等待 join 超时当作立即拒绝。修复后 relay 62 项常规用例和 all-targets Clippy、fmt/diff 检查通过；三个手工 probe 默认忽略。

已有 `e0d9fca502e5b2c663ace7ffcee7ab7a495bd90f` 初轮候选曾在 256MiB 本地容器、64 live connection 下取得 33398784 字节 cgroup 峰值和 0 OOM kill，但它不包含本节防护、053 或057，不能代替最新候选验收。最终候选复测见下节。057 的源连接关闭回归另证明可靠子任务不会在 admission 释放后继续持有旧缓冲；初轮容器测量不证明这个 churn 行为。

## 最终候选本地容器实测（2026-10-26）

测量主题：`test(DL2-052): verify candidate container resource limits`。Runtime 来自精确源码 `020c5b342aeb2ef2545339be799a8eb1a252c83c`，包含 052 两个防护切片、053、049 和057；完整 616 个 tracked 文件（含 `.cargo/config.toml`）已逐一对照该提交。Rust 1.97.1、Bun 1.3.14 固定镜像摘要，公共 Cargo 归档按锁文件 checksum 校验后离线构建，`cargo --offline --locked` 在禁网 RUN 内执行，静态链接检查通过。未复制用户 Cargo 配置或凭据，也未改锁中的包版本。

- 候选 tag：`desklink-relay:dl2-network-020c5b3`。
- Immutable image ID：`sha256:f5029e8400e61b0b80fe51e3f7a1d3fe75d1df7cbbcb39d991ee55d2d30a3847`。
- 镜像 revision label 为上述完整源码 SHA，平台 `linux/amd64`，runtime UID/GID `65534:65534`。
- 显式测试容器：256MiB 内存且无额外 swap、0.75CPU、PIDs64、只读、cap-drop ALL、no-new-privileges、仅映射 `127.0.0.1` 随机 UDP 端口。使用本夹具合成的临时 TLS 证书；禁用 healthcheck 以隔离负载，观察 helper 的短时开销仍计入 cgroup。

夹具先验证空/非空 datagram ×未join/partial join 四种连接均在 1 秒测试预算内收到 ApplicationClosed，再检查 uni 无 credit /bidi 上限并清理这些 peer。压力阶段使用 64 个存活连接：1 条健康会话（2 个 peer）、2 个已 join 的空闲 Host、30 对停读目标会话。每个压力来源先预写16个帧头，再并发写 body，避免客户端窗口被首个 body 占满而让其他帧头无法启动。

实际记录 scheduled/started writers 均 480，159 个 writer 完成首帧 `write_all`，30 个来源均有完成的客户端写，合计 171 个完整帧写完成；所有480个 writer仍有未完成工作，独立健康 Input 通道5轮传输均成功。**这些计数表示客户端发送 API 完成，不代表171帧全部已由服务端读取、授权或落盘。** 压力与背压条件由夹具分别观察，不用排队数量冒充实际服务端吞吐。

以下为夹具原始 observation JSON。`process_resident_bytes` 是同 cgroup 内短时观察进程的 RSS，不能称为 relay PID1 的 RSS。

```json
{"phase":"baseline","schema":1,"process_resident_bytes":2310144,"cgroup_current_bytes":6778880,"cgroup_peak_bytes":6778880,"cgroup_limit_bytes":268435456,"cgroup_oom_kills":0}
{"phase":"pressure","schema":1,"process_resident_bytes":2424832,"cgroup_current_bytes":34099200,"cgroup_peak_bytes":34099200,"cgroup_limit_bytes":268435456,"cgroup_oom_kills":0}
{"phase":"after_cleanup","schema":1,"process_resident_bytes":2375680,"cgroup_current_bytes":34402304,"cgroup_peak_bytes":34402304,"cgroup_limit_bytes":268435456,"cgroup_oom_kills":0}
```

该观察阶段的 cgroup 峰值为 **34402304 字节（32.81MiB）**，OOM kill 为0，服务保持运行。清理后 current 略升，没有宣称 heap/RSS 已立即归还系统；这也不是所有碎片模式、全部最坏组合、长稳或公网容量的证明。同一候选的实际 SIGTERM 与全零收尾记录见 [受控停机](relay-graceful-shutdown.md)。只创建并清理本任务唯一容器和临时 TLS 文件，没有操作现有服务或容器。

重现命令（显式手工 probe，普通测试默认 ignored）：

```powershell
$env:CARGO_TARGET_DIR='E:\desk-link\.worktrees\dl2-targets\network-lan'
$env:DESKLINK_RESOURCE_PROBE_IMAGE='sha256:f5029e8400e61b0b80fe51e3f7a1d3fe75d1df7cbbcb39d991ee55d2d30a3847'
$env:DESKLINK_RESOURCE_PROBE_REVISION='020c5b342aeb2ef2545339be799a8eb1a252c83c'
python scripts/run-windows-cargo.py test -p desklink-relay --test container_resource_probe --locked -- --ignored --nocapture --test-threads=1
```

完整通过日志保留于 `E:\desk-link\.worktrees\dl2-targets\network-lan\dl052-057-020c5b3-final-02.log`，SHA256 `F932416B8DE69ACCB2185803212C8CD71206BCA8A6856756DBD53D43CF42A0F3`。该次用例1通过、0失败，测试耗时7.66秒，相关 Clippy/fmt/diff 检查通过。初次最终候选运行在已完成压力观察后因 Docker stop 的弃用提示被严格输出断言误判，完整失败日志 final-01 保留；修正仅为接受提示并核对最后一行仍为 owned CID，没有放宽容器资源或通过条件，正式证据只引用 final-02。
