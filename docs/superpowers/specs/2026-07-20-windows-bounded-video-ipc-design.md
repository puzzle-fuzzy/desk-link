# Windows 0.1.59 有界视频 IPC 设计

## 背景与已确认问题

DeskLink 0.1.58 已在共享控制端阻止网络缺片或帧号断层后的无效 H.264 增量帧，并能有界请求关键帧。该保护发生在 QUIC 解密与视频重组之后。

Windows Tauri 边界仍使用 `Channel<Response>` 把每个完整 H.264 帧推送到 WebView。仓库固定使用的 Tauri 2.11.5 对超过 1024 字节的原始 Channel 负载，不会直接执行回调，而是把负载以递增 ID 插入 `ChannelDataIpcQueue<HashMap<u32, InvokeResponseBody>>`，再让 WebView 逐项 fetch。H.264 帧远大于该阈值，Rust 的 `Channel::send` 只完成入队，不等待 JavaScript 消费。

因此 WebView 主线程短暂卡顿时，旧视频帧和对应内存可以在 Tauri fetch 队列中持续累积。前端的 WebCodecs `decodeQueueSize` 只观察已经到达 JavaScript 的帧，无法发现这段 IPC 积压。

## 目标

- 消除大视频帧在 Tauri Channel fetch 队列中的无界积压。
- Rust 与 WebView 之间最多保留一帧正在响应和一帧等待交付。
- WebView 变慢时优先保持低延迟，不把旧帧完整排队后慢慢播放。
- 丢弃增量帧后必须关闭该参考链并有界请求关键帧，不能把依赖已丢帧的后续数据交给 WebCodecs。
- 视频流、配置版本、断线和重连必须严格隔离，旧拉取请求不能消费新会话帧。
- 不改变协议 9、relay、主机编码格式、用户画质设置或主界面。

## 方案比较

### 方案 A：浏览器单请求拉取 + Rust 单槽邮箱（采用）

控制端网络任务把完整帧写入一个绑定 `stream_id + config_version` 的单槽邮箱。前端收到视频配置后启动一个串行拉取循环，只有当前 `invoke` 返回并同步提交给解码器后才请求下一帧。

优点是自然限制在途数据，绕开大负载 Channel 的无消费确认问题；Rust 可以在邮箱满时根据关键帧属性安全决策。代价是增加一个异步 Tauri 命令和一套小型邮箱状态。

### 方案 B：保留 Channel 并增加 JavaScript 确认

Rust 每发送一帧后等待前端确认，再发送下一帧。要避免阻塞网络事件循环，仍然需要独立缓存、通知和溢出恢复；确认到达前的大负载已经进入 Tauri fetch 路径，边界更难证明。

### 方案 C：把视频拆成小于 1024 字节的 Channel 消息

可以走 Tauri 的直接执行分支，但 1080p H.264 每帧会产生大量 JavaScript 回调和重新组装工作，放大主线程压力，不符合本次目标。

## 架构

### 共享参考链状态

将 `VideoContinuity` 从 `desklink-ffi` 的私有模块移动到 `desklink-video` 并公开导出。网络重组边界和 Windows IPC 邮箱使用同一套规则：

- 关键帧建立新的连续性起点；
- 已知损失或帧号断层进入等待关键帧；
- 等待期间丢弃增量帧；
- 自动关键帧请求最多每 1 秒一次。

现有 0.1.58 单元测试随实现移动，FFI 的 localhost relay + Noise 集成测试继续覆盖网络侧接线。

### Windows 单槽视频邮箱

新增 `apps/windows-ui/src-tauri/src/video_mailbox.rs`：

- 邮箱键为 `(stream_id, config_version)`；
- 等待队列容量固定为 1；
- `begin_config` 切换键、清空旧帧、重置参考链并唤醒旧等待者；相同键重复到达不破坏当前状态；
- `offer` 先经过 `VideoContinuity`，再决定入槽、丢弃或请求关键帧；
- 槽满且新帧是增量帧时，保留已排队的安全帧，丢弃新帧并进入关键帧恢复；
- 槽满且新帧是关键帧时，用更新的自包含关键帧替换旧槽；
- `next` 只为完全匹配的流和配置返回一帧；切换配置或关闭邮箱会唤醒并拒绝旧请求。

邮箱返回 `Queued`、`Dropped`、`RequestKeyframe` 或 `Ignored`。网络任务收到 `RequestKeyframe` 时调用现有 `ControllerRuntime::request_keyframe`，失败仍按现有连接错误路径处理。

发生单槽溢出时，Windows 控制端还会把一次有界的本地新鲜度恢复合并到 0.1.57 已有的播放压力样本。自动画质因此可以在持续 IPC 消费不足时逐级降档；手动画质仍忽略该反馈。

### Tauri 与前端

- `connect_device`、`connect_saved_device` 和 `reconnect_controller` 不再接收大视频 Channel，只保留小信令 Channel 与音频 Channel。
- 新增 `next_controller_video_frame(stream_id, config_version) -> Response<Vec<u8>>` 命令。
- 前端 `createControllerChannels` 删除视频回调；收到新的 `videoConfig` 后启动严格串行的拉取循环。
- 每个拉取循环绑定前端 generation、`streamId` 和 `configVersion`。断线、取消、画质重配置、切屏或新连接都会使旧循环退出。
- 每次响应仍使用现有 17 字节前缀，因此 H.264 解析、WebCodecs、新鲜度恢复和画面绘制逻辑保持不变。

## 数据流

```text
QUIC datagram -> Noise 解密 -> 帧重组/网络连续性门
  -> Windows 单槽邮箱
     -> 浏览器唯一 in-flight invoke
        -> 现有 handleVideo/WebCodecs

邮箱满 + delta -> 保留槽内帧、丢当前帧、请求关键帧
邮箱满 + keyframe -> 用新关键帧替换槽内帧
```

## 错误与生命周期

- 拉取命令因断线、流切换或配置切换结束属于正常生命周期，前端静默退出旧循环，不显示错误提示。
- 畸形视频负载仍走现有 `handleVideoDeliveryError`，并请求关键帧。
- 每轮连接尝试开始和结束时关闭邮箱，唤醒等待中的命令；新 `VideoConfig` 再开启新键。
- 邮箱锁内不执行 IPC、网络 await 或解码工作；通知发生在状态更新后。
- 视频邮箱溢出只影响视频通道，不关闭远控，也不阻塞输入、声音、剪贴板和文件传输。

## 测试

- `desklink-video`：移动后的连续性测试必须原样通过。
- 邮箱单元测试：正常关键帧/增量帧、单槽上限、增量溢出请求、1 秒冷却、新关键帧替换、重复配置不重置、配置切换唤醒、关闭唤醒和错误键隔离。
- Tauri 控制端测试：连接接口不再需要视频 Channel；运行时配置开启邮箱；H.264 帧入槽；溢出动作请求关键帧。
- 播放压力测试：邮箱溢出合并一次本地新鲜度恢复，保持现有数值边界与手动画质隔离。
- TypeScript 纯状态测试：同一时间只允许一个拉取循环；新配置和断线 generation 使旧响应失效。
- 完整门禁：Bun、Python、Rust workspace、Clippy、生产中继协议 9 探针、x64 release 与安装器哈希。

## 发布与兼容

- Windows 产品版本升级为 0.1.59。
- `PROTOCOL_VERSION` 保持 9；网络格式和 relay 不变，无需服务器部署。
- 0.1.58 与 0.1.59 可以建立协议 9 会话，但 0.1.59 的控制端才具备有界 Tauri 视频 IPC。
- 两台电脑实际测试仍推荐同时升级到 0.1.59。
- 安装包继续作为未签名测试版本，正式签名边界不变。
