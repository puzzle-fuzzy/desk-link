# Windows 0.1.81：高频鼠标输入复用

## 目标

降低高轮询率鼠标在控制端 WebView2 主线程上的短命对象分配，避免鼠标移动与视频绘制同时发生时触发额外垃圾回收停顿。

## 改动

- `writeNormalizedPointerPosition` 将归一化坐标写入调用方提供的对象。
- `pointermove` 只更新这一份坐标存储，并由 `requestAnimationFrame` 合并为至多一条待发送移动消息。
- 点击、释放、滚轮、键盘、拖拽和断开时的输入释放逻辑保持原有顺序与语义。

## 验证

- `apps/windows-ui`: 146 项 Bun 测试通过。
- `bun run build`: TypeScript 检查与 Vite 生产构建通过。
- 需要在两台 Windows 实机上继续观察高轮询率鼠标、视频播放、双屏切换和重连时的输入延迟。
