# Windows 输入上下文（DL2-013）

Rust 和 wire 使用 `InputContext { stream_id: u64, input_epoch: u64 }`。Windows IPC 的这两个字段以及视频配置、运行状态、拉帧和诊断中的 stream 身份使用规范十进制字符串，例如 `{"streamId":"9007199254740993","inputEpoch":"18446744073709551615"}`。零值、数字 JSON、符号、小数、空格、前导零和 u64 溢出均拒绝；`configVersion`、尺寸等 u32 仍使用 number。

每个输入在产生或入队时固定已经显示的 pair；队列、合并、文字分片、快捷键释放和异步粘贴不会采用更新后的上下文。新配置使输入暂停，精确配置下的 `drawImage` 成功后，再通过可见 rAF 屏障核对 frame token、会话代数、stream、epoch 和配置版本，才能放行。解码 output 或收到 VideoConfig 本身不算显示确认。隐藏、零面积、离开会话、断开和迟到回调不能恢复旧 context。

同 epoch 的画质配置在原 DOM 上重配解码器，不清除 held keys；新 epoch 清除本地旧状态，释放事件仍保留旧 pair。帧和 mailbox 只携带 stream/configVersion，因此主机每次改变 epoch 必须同时推进 configVersion，禁止给旧帧重贴新身份。

Native 使用一个拥有 InputSender 的输入 future，沿原有有界命令队列回压；媒体、配置和会话取消继续在同一 select 被处理。当前完整输入帧发送后才检查是否继续文字/粘贴序列，避免取消半个可靠消息。失效 pair 属于本地操作取消，不变成远端权限限制或重连。粘贴等待远端 ACK 后仍使用原 pair，并保持 busy 到 Ctrl+V 序列完成。

自动验证覆盖真实绘制入口的调度与可见门禁、实际 IPC 参数、完整 u64、队列背压、新配置在输入 pending 时被消费，以及加密输入序号和上下文。联合回归另以真实 SoftwareOnly MFT 编码合成 BGRA，由 HostVideoPipeline 发布配置和 IDR，经 localhost QUIC/Noise 到 ControllerRuntime，再将捕获的 context 加密发送回 Host 映射门禁和记录后端；覆盖同尺寸切屏、新 Noise 会话及旧 release 拒绝。它不能证明浏览器合成器或物理屏幕已经呈现。真实 WebView 呈现、双机拓扑/输入法、FFI Mac Host epoch 接线和硬件验收仍保留未完成。联合证据见 `windows-joint-integration.md`。
