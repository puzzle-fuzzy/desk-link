# Windows 媒体分段性能基线

DL2-014 的首个可复跑切片只测本机合成输入，不打开远控连接、不捕获桌面、不注入输入。生产 `H264Encoder` 为最近一次提交保存单个固定尺寸样本，计时使用本机 `Instant`；配置切换清空样本。`convert_us` 包括 CPU 转色及其分配，`mft_us` 包括同步 MF 提交和可用输出的读取；输出驻留时间从对应输入的本机提交开始计算。它不是跨机帧龄、GPU 时间或输入到显示延迟。

```powershell
$env:CARGO_TARGET_DIR = 'E:\desk-link\.worktrees\dl2-targets\windows-media'
python scripts/benchmark-windows-media.py --width 1920 --height 1080 --frames 120 --warmup 10 --label 'same-machine-static' --output dist/windows/media-1080p-before.json
python scripts/benchmark-windows-media.py --width 2560 --height 1440 --frames 120 --warmup 10 --motion --label 'same-machine-motion' --output dist/windows/media-1440p-before.json
```

需要本机实际编码时显式加 `--encode`。合成 BGRA fixture 的复制发生在生产分段计时之外；报告保留包含 warmup 的总墙钟时间，不能把它解释为实时 FPS。默认 release；`--profile dev` 只用于工具烟测，不能和 release 比较。静态边缘和逐帧变色各运行至少三次，同一 CPU/GPU、电源/温度状态、工具链、分辨率、profile、码率与帧数下保存优化前后报告。报告记录运行前后的提交、dirty 状态与文件摘要、环境标签及 p50/p95/p99/max；只有 `source_unchanged_during_run=true` 的报告才用于对照。`known_*_bytes` 只计当前明确的 NV12 写入与 MF 输入复制，不能当作驱动复制总量或 allocator 采样。未实现的采样使用 `null`，不填 0。

后续完整验收仍需真实双机的 capture→queue→transport→assemble→decode→display 和 input→inject 关联；使用同一会话代数与帧号。不得直接减去两台机器的系统时钟；端到端用外部高速摄影/回显，或给出时钟校准误差。至少记录 Intel/AMD/NVIDIA/混合显卡、1080p/1440p、静态文字/滚动/视频、LAN/中继、带宽阶跃矩阵的 CPU/GPU/内存、功耗/温度、分配率、队列年龄、音画延迟和长稳斜率。当前切片不能关闭这些人工验收项，也不证明任何性能收益。
