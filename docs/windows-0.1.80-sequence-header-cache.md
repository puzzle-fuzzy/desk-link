# Windows 0.1.80：控制端序列头缓存

## 问题

控制端收到的视频配置中的 SPS/PPS 是 Tauri IPC 反序列化后的 `number[]`。此前每个访问单元进入 WebCodecs 前都会执行 `new Uint8Array(videoConfig.sequenceHeader)`；30 FPS 会话中，这个固定不变的序列头会反复制造短生命周期数组和垃圾回收压力。

## 调整

- 视频配置的流 ID 或配置版本变化时，将序列头转换为一个 `Uint8Array`。
- `prepareH264AccessUnit` 以及 `VideoDecoder.configure` 都复用缓存，不再按帧复制 SPS/PPS。
- 新会话、终态断开和不保留远程画面的恢复路径会清空缓存，避免旧配置被误用。

## 边界与验证

该改动只改变控制端本地内存生命周期，不改变访问单元内容、关键帧判断、H.264 codec 字符串、WebCodecs 解码顺序或协议 9。前端 142 项测试、Rust workspace、Clippy 和 Windows 发布门禁继续作为验证门槛；真实双 Windows 的 WebView2 垃圾回收和帧间隔仍需人工观察。
