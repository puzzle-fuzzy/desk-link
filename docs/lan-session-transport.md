# DL2-024：会话业务数据面与逐通道切换

本记录对应第二波 transport 实现。Windows/FFI 生产接线与真实文件夹具见 [生产接线记录](lan-runtime-integration.md)；第一波 `LanDataPlane` 的历史验证仍保留在 [原组件记录](lan-data-plane.md)。没有把 opt-in 构造器、客户端 write_all 完成或传输收讫当作生产协商、输入执行或文件落盘证明。

## 唯一合同与生命周期

业务协议 14 只复用文件 owner 来源 `5decea41a272ac1bcd42590b97a66fe02cc5e3e6`、`4d8ea4025a81c3797df87af94276daf35a9724c4`。没有新业务版本、输入格式、Noise 算法或 JOIN 信封。两端先在 Noise 认证后的真实 DeviceCapabilities 交换里确定 `lan_data` 交集，冻结每个通道的正文模式；None/无交集使用原格式，未选中通道也继续原格式，不能仅凭 probe 成功启用 LAN 可靠消息。

`NegotiatedLanData` 核对双端能力、角色、Noise binding 和本地实际 relay peer generation；`SessionDataPlane::relay_only` 再核对 SecureSession 与 backend 的绑定。`QuicRelayBackend` 固定原 peer generation，不跟随新的 relay peer 转发旧密文。切路由的 switch generation 是双方共享的独立单调计数，不能等同于两端各自看到的 relay connection ID。

`LanSwitchNegotiator` 对同一有效候选、代数和通道集合执行 Offer → Ready → Commit，完成后才产生字段私有的 `LanAttachAuthority`；attach 还核对认证 DirectLanConnection 的候选/binding，以及当前代数。生产中 Controller 主动 probe Host 候选，Host 是唯一切换提议者；完整会话只允许一个 reader 拥有每条 lane，旧视频 reader 不与 DLD1 争读。

## 可靠 lane 的远端接收屏障

本地 relay 写完成不等于对端已接收入队，不能据此让新 LAN 消息越过旧 relay backlog。Commit 之后先安装认证 reader，再分别为每条获准可靠 lane 建立屏障；不同 lane 不持有彼此的发送锁。

1. 保持该 lane 的顺序锁，经 relay 发送内部 AEAD 明文 `DLF1` + switch generation（u64 大端）+ 随机 128bit token。它是 28 字节，密文 52 字节，relay length 加密文共 56 字节；不作为业务正文交给消费者。
2. LAN 双向流发送同一完整密文作为 query。DLD1 channel 字节高位 `0x80` 表示 query，其余位仍是原 ChannelKind。
3. query **不会 open 新密文或推进 replay window**。它只等 relay ingress 已实际接收的完全相同密文，并同时匹配该连接不可变的 committed mask 和 switch generation，之后才回 token、允许此 lane 的 LAN 数据。
4. sender 验证 token 后仅把此 lane 切为 Direct；屏障超时保留 Relay。旧代数 fence、未批准通道、未知密文均不能放开 lane；单纯清缓存无法防止迟到旧 backlog，所以代数在 AEAD 正文内绑定。

正常可靠业务仍用 `DLB1` + 随机 128bit token + 原业务正文，可靠业务正文上限 65491 字节；LAN datagram 正文上限 1170 字节。InputContext 是业务正文的一部分，数据面不读取或重贴它。LAN 收讫只表示认证及有界入队；回退发送同一密文，统一 ingress 去重，不重新 seal。

## 背压与关闭

五条可靠 relay lane 各有一个 owned writer 和一个排队槽。调用方超时/取消只放弃结果等待，已开始的 QuicClient 完整帧仍由原 worker 驱动；未开始且调用方已经退出的请求跳过。可靠业务超时返回带 lane 的 Interrupted，屏障超时则保留 Relay，不能混用这两种语义。媒体不为每包新建 task。

健康状态逐 lane 记录 Relay/Direct/Unavailable/Interrupted。双方允许延续且真实 LAN 的 Control/Input 已通过屏障时，relay 离线可继续既有 Noise 会话；可选文件/音频失效单独降级，不静默授权新设备或重新绑定密文。own relay 仍活但旧 peer 丢失 relay 时，Control reader 持续观察新 generation；新 peer 出现会关闭旧 plane 并保留新握手供 Host 重新认证。

DirectLan 双端使用现有 5 秒 QUIC keepalive、15 秒 dead timeout，静态画面/静音不再以无视频消息误判失效。PING 不是应用消费回执。关闭用注册锁阻止新任务，`close_and_wait()` 等待五个 writer、八个 relay reader、direct/fence 与 accepted-stream 子任务，计数也覆盖尚未首次 poll 就取消的任务。没有 Arc 自持循环。

保留原可靠帧安全边界：正常 idle 关闭不关闭 durable relay；若强制结束仍在部分写入的会话，QuicClient framing guard 仍可能关闭物理 relay，此时需要重新连接/认证。繁忙 replacement 不承诺保留同一个物理 relay；不移除该 guard 来伪装无损切换。可靠通道之间仍共享 QUIC 拥塞/连接预算，没有宣称硬带宽预留或任意小窗口下的延迟保证。

## 已实际运行的本地验证

Rust 1.97.1，仓库 `scripts/run-windows-cargo.py`，`--locked`，独立 target `E:\desk-link\.worktrees\dl2-targets\lan-runtime`：

- transport 49 单元 + 23 localhost 通过；包含 early-query 不 burn 序号、committed-mask 越权拒绝、同 Noise 的 gen1 fence 不得放开 gen2。
- `business_transport` 十项常规覆盖真实 relay、实际不同 peer generations、Noise/probe、256 条旧 Input 的屏障顺序、逐 lane 回退/同密文去重、可选 Transfer 超时、替换 peer、新旧任务回收。
- 独立 17 秒静默用例实际 `--ignored` 执行，1 passed，测试阶段 17.04 秒。先断测试独占 relay，静默超过配置的 15 秒，再验证 Input/Control 仍可达；默认 ignored 不计作常规通过。
- 两项 raw-sink 背压负例实际通过：MAX 帧 caller 取消后完整 drain，排队取消请求跳过；另一项 source send 7B、stream receive 48B（48+7<实际 fence 56B），保持 300ms 操作预算，证明 fence 超时不取消原帧、同 relay 核心加密收发仍可用。该回归不是 Host 输入注入或文件落盘证明。

原始日志在同一 target 父目录：`lan-runtime-transport-final-01.log`、`lan-runtime-business-final-01.log`、`lan-runtime-silent-final-01.log`、`lan-runtime-owned-backpressure-final-03.log`。`owned-backpressure-final-02` 的 source send 4B 设计令 Input 超过其 300ms 夹具预算，已保留失败；正式第三轮使用仍严格小于完整 fence 的 7B，不增加 deadline。初轮 2048B send buffer 不能仅凭 receive window 推断 fence write 被卡，未用作该证明。

以上是 loopback/合成业务与真实协议/流控的边界回归；不是双机吞吐、音频播放/屏幕呈现、真实 OS 注入、NAT 穿透、容器或线上容量结论。
