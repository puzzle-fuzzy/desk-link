# Windows 0.1.79：数据报原地加密

## 目标

0.1.78 已经让视频编码直接产出明文数据报，但每个数据报进入端到端加密时，旧实现仍会先由 AEAD 创建一个完整密文 `Vec`，再创建带 8 字节序列号前缀的第二个 `Vec` 并复制密文。30 FPS 视频通常每帧包含多个数据报，这个复制会放大堆分配和 CPU 抖动。

## 本次调整

- `PacketCipher::seal` 使用带前缀的缓冲适配器：序列号保持在缓冲区头部，AEAD 只对后面的明文区域原地追加认证标签。
- 发送结果仍然是 `sequence || ciphertext || tag`，调用方、QUIC relay 和接收端无需改变。
- 现有 `SecureSession` API、每 lane 独立序列号、重放窗口和错误处理保持不变。

## 安全边界

序列号仍然进入 nonce 和 AAD，且不会被加密；只有明文 payload 区域经过 ChaCha20-Poly1305 保护。原地缓冲区只在单次 `seal` 调用内存在，不会跨会话或跨 lane 复用，避免把密钥、序列号或未确认明文带入其他数据报。

## 验证

Crypto handshake、双向 secure session、重放/篡改/跨 lane 测试均通过。由于 AEAD 输出格式保持一致，现有协议、视频、FFI、Windows 和端到端恢复测试继续适用；真实双 Windows 长稳仍需人工验收。
