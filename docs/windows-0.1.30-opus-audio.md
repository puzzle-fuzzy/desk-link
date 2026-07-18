# DeskLink Windows 0.1.30：Opus 系统声音

## 目标

0.1.29 已经打通 Windows 默认输出设备的 WASAPI loopback、独立加密音频数据报和控制端 WebAudio 播放，但原始 48 kHz 单声道 PCM 需要约 768 kbit/s。0.1.30 在不改变界面和中继协议职责的前提下，将这条可选链路压缩为低延迟 Opus，避免声音在公网质量不佳时与画面争抢带宽。

## 实现

- 主机继续以 10 毫秒、48 kHz、单声道、16 位 PCM 从 WASAPI 队列取帧。
- Windows 原生层使用 vendored libopus 静态编码，目标码率 64 kbit/s、受约束 VBR、复杂度 6，并声明 5% 预期丢包。
- 编码后的 Opus 帧通常约 80 字节，协议硬上限为 512 字节；原始 PCM 接收能力仅保留为更新过渡，不再由新版主机发送。
- 音频仍使用独立的 Noise 加密 QUIC 数据报。中继只转发密文，不解析编码类型，因此本版本不需要更新服务器。
- 控制端在 Rust 层解码 Opus，再沿用已有的 10 毫秒 PCM IPC 和 WebAudio 调度器，浏览器层无需依赖 WebCodecs 支持。
- 当只丢失一个连续数据报时，控制端使用下一包的 Opus in-band FEC 恢复缺失的 10 毫秒帧；重复/迟到包会被丢弃，连续丢失多包时重置解码状态和播放缓冲。
- 编码器、解码器或音频数据报拥塞只会停用/丢弃声音，不会结束画面、输入、剪贴板或文件传输会话。

## 构建可重复性

`opus-head-sys 0.3.1` 将固定版本的 libopus 源码包含在 crate 中，不在构建期间下载第三方二进制。Windows 构建固定使用 Ninja，避免同一台机器同时安装稳定版和预览版 Visual Studio 时 CMake 自动选择错误的 MSBuild 生成器；发布脚本还会为整个应用启用静态 MSVC CRT，安装后的电脑不需要额外的 Opus DLL 或 Visual C++ Redistributable。

发布脚本会在 Ninja 未加入 `PATH` 时自动查找 Visual Studio 自带的 `ninja.exe`。开发机如果直接执行 Cargo，应先让 Ninja 可用，或通过项目的 Python 发布/验证脚本运行。

## 自动验证

- 协议测试覆盖 PCM/Opus 往返、编码类型约束和 Opus 512 字节上限。
- Windows 单元测试覆盖 10 毫秒编解码、压缩体积、单帧 FEC 恢复和固定 PCM 输出长度。
- Tauri 控制端测试覆盖单包缺口恢复、顺序重建和重复数据报丢弃。
- 发布门禁继续执行 Bun 前端测试/构建、Rust workspace 测试、Clippy、格式检查、x64 PE 检查及安装包内嵌载荷哈希验证。
