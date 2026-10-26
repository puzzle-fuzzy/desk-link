# DL2-057：relay 受控停机（Rust 切片）

修复主题：`fix(DL2-057): drain relay tasks and notify peers on shutdown`。

## 停机行为与预算

relay binary 在开始监听前同步注册 Unix SIGTERM/SIGINT 或 Windows Ctrl+C。`--check-config` 和 `--resource-observation` 在注册前直接返回，不绑定监听地址，也不等待信号。

收到信号后，`close()` 先设置停止标记，再关闭 QUIC endpoint。已经连接的正常 peer 收到应用层 CONNECTION_CLOSE，原因固定为 `relay shutdown`；不靠 15 秒传输 idle 超时发现停机。Quinn 关闭后仍可能从旧队列返回 incoming，因此 `run()` 不再登记它们，并显式拒绝剩余队列。

`run()` 用 JoinSet 跟踪 admitted connection 和 rejection handler，每个连接用自己的 JoinSet 跟踪可靠转发子任务及媒体 worker。正常关闭、连接被替换或源连接单独离线时，子任务均先取消并等待，之后才释放 participant/admission。run future 被取消时 Drop guard 也会请求关闭 endpoint；**需要等待 run 完成，才能确认收尾完成**。

`RelayConfig.shutdown_timeout` 默认 **7 秒**，主进程沿用此值。最后最多 1 秒保留给强制取消与 guard 回收，应用任务和 QUIC `wait_idle()` 共用总预算。主进程持续驱动 run future，收尾后再取消并等待 metrics task；不是收到信号就立即结束 runtime。候选 Compose 现有 `stop_grace_period: 10s` 未变，正常默认停机预算在此范围内。宿主若修改库级 shutdown_timeout，应同步核对自己的外部终止预算。

固定字段 `relay_shutdown` 日志区分 `graceful`、`forced`、`timed_out`，记录 admission/participant/media/reliable/rejection 计数、强制任务数量、`endpoint_idle_confirmed` 与收尾耗时。只有确认资源计数为零、endpoint 已 idle 且无需强制终止连接 handler 才返回成功。强制或未完成清理返回 `RelayError::ShutdownTimeout`；不能把它记录成 graceful。QUIC 关闭后在途媒体/文件发送会中止，本项保证受控清理及对端通知，不承诺每个在途消息已经被业务消费。

## 源连接生命周期与资源模型

此前可靠转发是 detached task。源连接关闭而目标仍正常 ACK、但停止读流时，任务可能继续等目标写入，同时持有原消息 Vec；源 admission 已释放也不能证明该缓冲已回收。057 把可靠子任务收进连接所有者，使任务生命周期符合 DL2-052 的每 admitted connection 缓冲模型。

新增回归同时建立两条最大帧可靠流，目标停读产生真实背压后**只关闭源连接**。验证 reliable worker 从 2 归零、admitted connection 从 4 降为 3、目标仍存活且另一健康会话 Input 可达；最后全局关闭时所有计数为零。这里以任务 guard 与生命周期证明旧任务不再持有消息，不把它描述为已测 RSS 立即下降，也不把旧 052 镜像的测量冒充这一修复后的 churn 证据。

## 已运行的 Windows 本地验证

使用仓库 Rust 1.97.1、`scripts/run-windows-cargo.py`、`--locked`，独立 target 为 `E:\desk-link\.worktrees\dl2-targets\network-shutdown`：

```powershell
python scripts/run-windows-cargo.py test -p desklink-relay --lib --bins --test graceful_close --test process_shutdown --test read_deadlines --test session --test resource_bounds --test address_family --locked
python scripts/run-windows-cargo.py clippy -p desklink-relay --lib --bins --test graceful_close --test process_shutdown --test read_deadlines --test session --test resource_bounds --test address_family --locked -- -D warnings
```

**58 项通过**：lib 12、bin 1、IPv4/IPv6 2、graceful_close 4、process CLI 1、deadline 7、resource 3、session 28；2 个已有手工 probe 默认忽略。新增生命周期回归还覆盖未 join 连接、可靠流背压、媒体、卡在 TLS 的拒绝任务、重复 close、新 admission 不可用、正常 peer 的 ApplicationClosed，以及预算耗尽必须返回超时。对应 Clippy、格式及 diff 检查通过。

signal feature 仅加在 relay 的 Tokio 依赖。首次 `--locked` 因新增 feature 依赖边拒绝后，离线 check 只增加 Cargo.lock 中 `tokio -> signal-hook-registry` 这一条边；已锁定包和版本均未升级，后续检查恢复 `--locked`。

## 已运行的 Linux 与候选容器信号验收

`process_shutdown.rs` 在 Unix 上另有真实子进程回归：启动同次 Cargo 构建的 relay binary，完成本地 QUIC join，对该测试独占 Child PID 发送 SIGTERM，验证 peer 及时收到 `ApplicationClosed`、子进程成功退出，且日志中 graceful 与各计数归零。信号通过 POSIX kill FFI 发送，常量来自 Tokio，**不需要 shell、独立 kill executable 或新依赖**。子进程 stdout/stderr 持续消费，超时只终止本夹具创建的 child。

在包含精确源码 `020c5b342aeb2ef2545339be799a8eb1a252c83c` 的 Linux builder 中，使用仓库 Rust 1.97.1 执行：

```sh
cargo +1.97.1 test --offline --locked -p desklink-relay --test process_shutdown sigterm_closes_a_normal_quic_peer_and_exits_the_real_binary -- --nocapture --test-threads=1
```

结果为 **1 passed、0 failed、0 ignored**，测试阶段 0.10 秒。`CARGO_BIN_EXE_desklink-relay` 由该次 Cargo 构建绑定真实 debug binary；这证明 Linux 主程序真实信号路径，不把它描述为静态候选 binary 的替代验收。builder 的该次执行禁用网络，源码上下文与锁文件核对见资源记录。原始日志：

- `E:\desk-link\.worktrees\dl2-targets\network\dl2-network-020c5b3-linux-sigterm-debug.log`
- SHA-256：`6A96A37277AC3A8EEB63F931322156F6925303B1693B0DE8B79320B0A6A0DF2C`

另对同源静态候选镜像 `sha256:f5029e8400e61b0b80fe51e3f7a1d3fe75d1df7cbbcb39d991ee55d2d30a3847` 运行 `container_resource_probe.rs`。在 256MiB、0.75CPU、PIDs64 限制下，完成 DL2-052 的 64 连接、480 started writers 压力与清理后，重新加入一个正常 peer，再对夹具独占的容器 ID 执行 `docker stop --time 10`。实测 peer 收到 `ApplicationClosed`，原因 `relay shutdown`；容器 exit code 0、未 OOM，日志为：

```text
relay_stop_requested signal=sigterm
relay_shutdown outcome=graceful accepted_connections=0 attached_participants=0 media_workers=0 reliable_workers=0 rejection_tasks=0 forced_tasks=0 endpoint_idle_confirmed=true elapsed_ms=85
```

**85ms 只是服务端 finish_shutdown 阶段，未测 Docker 命令或信号发出至退出的端到端耗时。** 整个组合夹具 1 passed，耗时 7.66 秒；其资源计数是已记录负载下的观察，客户端 write_all 完成数不等于服务端完整收帧数。cgroup 观测包含短时 observer，其 process RSS 不是 relay PID 1 RSS；完整资源 JSON 与边界见 [relay 资源记录](relay-resource-bounds.md)。原始通过日志：

- `E:\desk-link\.worktrees\dl2-targets\network-lan\dl052-057-020c5b3-final-02.log`
- SHA-256：`F932416B8DE69ACCB2185803212C8CD71206BCA8A6856756DBD53D43CF42A0F3`

`final-01` 因 Docker 29 把 `--time` 弃用提示写到 stdout，夹具误把整段输出当成容器 ID 而失败；修正为核对最后一行后，`final-02` 才是正式通过证据。未放宽资源限制，失败日志保留。测试只清理其独占容器和临时 TLS 文件，未部署。

本网络切片保持业务协议 13 与 relay JOIN wire 不变；文件业务协议 14 由文件任务负责。实际 Windows Ctrl+C 和 Bun 服务关闭仍待各自验收，整个 DL2-057 保留未完成。
