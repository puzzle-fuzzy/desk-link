# Windows 0.1.78：视频数据报直接编码

## 目标

在主机端的 `DXGI → H.264 → QUIC` 热路径中，编码帧已经是一段连续的访问单元。旧路径先把每个 1 KiB 分片复制到拥有数据的 `VideoPacket`，再在序列化前复制一次 payload，最后才生成待加密的数据报。高分辨率画面下，一个访问单元可能包含几十个分片，这会制造大量短生命周期堆分配。

## 本次调整

- 协议层新增 `encode_video_packet_parts`，验证相同的头部、长度和数据报上限，但让 postcard 直接序列化借用的 `&[u8]`。
- `desklink-video::encode_video_frame` 直接遍历访问单元分片并生成加密前的数据报，不创建中间 `VideoPacket` 或 payload 副本。
- Windows 原生主机发送循环和 FFI host worker 都切换到该路径。
- `packetize_frame` 保留，用于接收组装、协议测试和仍需要拥有 `VideoPacket` 的调用方；其行为和 wire bytes 不变。

## 分配边界

对每个视频分片，旧发送路径会额外创建 packet payload 副本并在编码入口再次复制；新路径只保留最终数据报的序列化分配。数据报集合本身仍然有界，之后还会进入既有的端到端加密发送流程。

## 兼容性与验证

新旧编码结果通过解码后的 `VideoPacket` 和逐字节 wire bytes 对照测试。协议、视频、Windows 核心和 FFI 测试均通过；协议 9、QUIC 数据报预算、关键帧恢复和中继无需升级。真实 Windows 双机的 CPU、帧率、GC 和网络抖动仍需人工长稳测试。
