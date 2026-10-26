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

## 待执行的 Unix 信号验收

`process_shutdown.rs` 在 Unix 上另有真实子进程回归：启动同次 Cargo 构建的 relay binary，完成本地 QUIC join，对该测试独占 Child PID 发送 SIGTERM，验证 peer 及时收到 `ApplicationClosed`、子进程成功退出，且日志中 graceful 与各计数归零。信号通过 POSIX kill FFI 发送，常量来自 Tokio，**不需要 shell、独立 kill executable 或新依赖**。子进程 stdout/stderr 持续消费，超时只终止本夹具创建的 child。

在包含本次精确源码的 Linux builder 中执行：

```sh
cargo test --locked -p desklink-relay --test process_shutdown sigterm_closes_a_normal_quic_peer_and_exits_the_real_binary -- --nocapture --test-threads=1
```

`CARGO_BIN_EXE_desklink-relay` 由该次 Cargo 构建绑定真实 binary，不能拿旧 052 镜像替代。当前 Windows 本机未执行这个 `cfg(unix)` 用例，也未做实际 Windows Ctrl+C、候选容器 SIGTERM 或线上部署。本提交只完成 Rust 代码和对应 Windows 自动验证；Bun 服务及整个 DL2-057 的关闭状态由各自验收汇总。
