# DL2-019 中继媒体调度切片

修复主题：`fix(DL2-019): isolate relay media queues from connection reads`。

## 实现边界

每条已加入会话的 QUIC 连接有三个常驻媒体 sender。连接读取循环只校验路由并非阻塞入队，不再等待 datagram 转发槽，也不再为每包创建 Tokio task。已建立的可靠流继续使用原有独立转发任务，每连接仍最多 16 个；本次解决的是媒体拥塞时新可靠流接受和连接关闭处理受阻。

| 通道 | 队列容量 | 入队到发送完成的总预算 |
| --- | --- | --- |
| 音频 | 32 包 | 60 ms |
| 光标 | 8 包 | 40 ms |
| 视频 | 256 包 | 250 ms |

每包路由头和载荷最多 1209 字节。三条队列及三个在途包合计最多 361491 字节载荷；这不包含 Quinn 自身缓存、连接对象、队列节点及暂处于读取循环的包。容量或时效耗尽时丢弃当前包并记录固定原因，已经入队的包保持 FIFO。发送等待沿用 Quinn 的不驱逐旧包接口，且排队与发送共享同一绝对 deadline。退出连接循环会取消并释放三个 sender。

路由在入队时绑定目标连接代数，出队再次确认；未指定代数的 bootstrap 包也不会因排队被重新定向到替换后的对端。能取得独占所有权时原地替换 `Bytes` 路由头，不能时才复制。

`RelayServer::media_snapshot()` 只返回聚合计数：各通道入队、满队列、过期、发送超时、已交给 Quinn 的包数、出队最大年龄及当前 sender 数量。已交给 Quinn 不等于对端已收到。各字段为独立原子采样，不保证跨字段事务快照。

## 验证与保留项

验证使用仓库 Rust 1.97.1、`scripts/run-windows-cargo.py` 和 `--locked`。目标目录固定为 `E:\desk-link\.worktrees\dl2-targets\network`。没有运行公网探针、发布或真实远控。

```powershell
$env:CARGO_TARGET_DIR='E:\desk-link\.worktrees\dl2-targets\network'
python scripts/run-windows-cargo.py test -p desklink-relay --lib --tests --locked
python scripts/run-windows-cargo.py clippy -p desklink-relay --all-targets --locked -- -D warnings
python scripts/run-windows-cargo.py fmt -p desklink-relay -- --check
```

队列单元回归覆盖视频饱和时音频/光标/另一会话仍可入队、满队列保留原 FIFO、关闭 worker 拒绝入队及绝对年龄预算。

本地 QUIC 回归通过 UDP proxy 在完成 join 后屏蔽慢端到中继的 ACK，再持续供给合法视频；明确观察到 `video.send_timeouts > 0` 后才判断发生真实发送背压。随后音频仍能进入独立队列，另一会话的输入/控制正常，新可靠流被接受，关闭后 12 个 sender 及连接容量全部回收。测试不以停止读取 datagram 冒充 QUIC 流控。

实际结果：relay 5 项 lib、1 项 bin、27 项 session 全通过，Clippy 所有目标零警告，rustfmt 和 `git diff --check` 通过。以上是本地合成故障结果，不是实机网络容量数据。

尚未完成 DL2-019 整体验收：中继只看到端到端密文，无法判断帧边界、关键帧或 delta 依赖，因此本切片没有实现按帧/依赖丢弃。三个 sender 仍共享目标 QUIC 连接的拥塞窗口与发送缓存，独立应用队列不能保证物理链路堵塞时音频可达。真实输入/音频 p95/p99、CPU、吞吐、多会话容量和长稳数据仍需测量；此处预算是策略参数，不是实测性能结论。
