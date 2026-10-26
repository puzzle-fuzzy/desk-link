# DL2-053：relay 应用层读取截止时间

修复主题：`fix(DL2-053): expire incomplete relay joins and reliable frames`。

## 截止时间模型

未认证连接从 admission 开始共享一个绝对 join 截止时间，默认 **8 秒**。它覆盖 QUIC/TLS 握手、等待首条 bidi 流、读取请求长度及正文、join/目录查询/拒绝回复的写入。收到新字节、QUIC keepalive 或 ACK 都不会重置该截止时间。超时关闭连接并释放 admission；如果已登记 participant，但回复因对端零接收窗口而阻塞，取消时也会撤销这次登记。登记和 admission 都由 Drop guard 管理。

认证后的可靠通道采用不同规则：

- 已接受流的 9 字节 routing header 必须在 **30 秒**内读完；Quinn 在流实际发送数据前不会向远端交付 `accept_bi`。
- routing header 完整后，等待下一帧第一个 length 字节没有应用层空闲超时。
- 首个 length 字节到达后，余下 3 字节长度与整个 body 共享一个 **30 秒绝对截止时间**，每次部分读取不会续期。64KiB 长度上限在分配 body 前检查。
- 完整帧之间可以正常静默；帧间 EOF 只结束该 lane。半帧超时或截断关闭违规源连接，使既有客户端恢复流程能释放输入并重建会话；其他会话继续运行。

这避免把“用户暂时不移动鼠标、不按键”误判为异常，也防止每隔几十秒滴入一个字节就一直占据半帧缓冲。读取 helper 使用带显式 offset 的 `AsyncRead::read` 循环。错误或取消后丢弃该 stream，不能从失去边界的位置继续解析。现有可靠转发任务和媒体预算保持独立。

`RelayConfig.join_timeout` / `partial_frame_timeout` 可供宿主配置；必须非零且能表示单调时钟截止时间。二进制环境变量分别为 `DESKLINK_RELAY_JOIN_TIMEOUT_S`（默认 8）和 `DESKLINK_RELAY_PARTIAL_FRAME_TIMEOUT_S`（默认 30），接受 1～100000 的整数秒。没有修改或部署线上服务。

## 本地自动验证

在 Windows 仓库工具链 Rust 1.97.1 下，使用 `scripts/run-windows-cargo.py`、`--locked`，独立 target 为 `E:\desk-link\.worktrees\dl2-targets\network-deadline`：

```powershell
python scripts/run-windows-cargo.py test -p desklink-relay --lib --bins --test read_deadlines --test session --test resource_bounds --locked
python scripts/run-windows-cargo.py clippy -p desklink-relay --lib --bins --test read_deadlines --test session --test resource_bounds --locked -- -D warnings
```

**51 项常规测试通过**（lib 12、bin 1、deadline 7、resource 3、session 28），另有 2 个手工 probe 默认忽略；对应 Clippy、格式与 diff 检查通过。

新增 helper 用例区分正常 EOF/截断、零长度、超长、帧间静默、滴字节不续期及 length/body 共用预算。真实 QUIC 用例覆盖正常 ACK keepalive 但不发送应用数据、半 join length/body、零回复窗口导致已登记 participant 挂起、半 routing header/length/body 仅关闭违规源、同一 Input lane 在 header 后及完整帧之间静默超过测试预算仍可继续发送，以及正常分段、缓慢到达的 join/可靠帧在预算内成功。未注入真实系统输入。

## 512 个静默连接的本机观测

手工执行：

```powershell
python scripts/run-windows-cargo.py test -p desklink-relay --test read_deadlines silent_512_connection_admission_cleanup_probe --locked -- --ignored --nocapture --test-threads=1
```

2026-10-26，Windows 11 build 26200，i7-13700KF（16 核 /24 逻辑处理器），debug 构建、4 个 Tokio worker、loopback。同一 client Endpoint 分批保持最多 16 个握手并发，遵守 DL2-052 的 64 个 pending incoming 上限。此夹具明确配置 512 accepted connection /session 上限、**2GiB 声明缓冲预算**，经资源模型验证；没有把生产默认 64 连接或 256MiB 容器改为 512 连接。join 使用默认 8 秒，keepalive 配为 100ms。

| 实际观察 | 结果 |
| --- | ---: |
| 峰值 accepted connection /已登记 participant | 512 /0 |
| 建立整个连接批次 | 573.274ms |
| 收到 keepalive PING 的 peer | 512 /512 |
| client connect 发起至收到应用关闭的 p95 | 8020.018ms |
| 同口径 p99 /最大值 | 8024.498ms /8025.068ms |
| 首次 client connect 发起至 admission 全部归零 | 8577.337ms |
| 最后 accepted connection /媒体 worker | 0 /0 |

这是本机应用 deadline 清理观测。各 peer 的时间从 client connect 发起计，包含网络、握手和调度，不等同于服务端逐连接精确 admission 时间；8.577 秒也包含 0.573 秒分批建连。不是公网容量、容器 RSS、CPU 收益或真实设备端到端延迟结论。本次未运行公网探针、服务部署或真实远控。
