# Windows 媒体分段性能基线

DL2-014 的首个可复跑切片只测本机合成输入，不打开远控连接、不捕获桌面、不注入输入。生产 `H264Encoder` 为最近一次提交保存单个固定尺寸样本，计时使用本机 `Instant`；配置切换清空样本。`convert_us` 包括 CPU 转色及其分配，`mft_us` 包括同步 MF 提交和可用输出的读取；输出驻留时间从对应输入的本机提交开始计算。它不是跨机帧龄、GPU 时间或输入到显示延迟。

```powershell
$env:CARGO_TARGET_DIR = 'E:\desk-link\.worktrees\dl2-targets\windows-media'
python scripts/benchmark-windows-media.py --width 1920 --height 1080 --frames 120 --warmup 10 --label 'same-machine-static' --output dist/windows/media-1080p-before.json
python scripts/benchmark-windows-media.py --width 2560 --height 1440 --frames 120 --warmup 10 --motion --label 'same-machine-motion' --output dist/windows/media-1440p-before.json
```

需要本机实际编码时显式加 `--encode`。合成 BGRA fixture 的复制发生在生产分段计时之外；报告保留包含 warmup 的总墙钟时间，不能把它解释为实时 FPS。默认 release；`--profile dev` 只用于工具烟测，不能和 release 比较。静态边缘和逐帧变色各运行至少三次，同一 CPU/GPU、电源/温度状态、工具链、分辨率、profile、码率与帧数下保存优化前后报告。报告记录运行前后的提交、dirty 状态与文件摘要、环境标签及 p50/p95/p99/max；只有 `source_unchanged_during_run=true` 的报告才用于对照。`known_*_bytes` 只计当前明确的 NV12 写入与 MF 输入复制，不能当作驱动复制总量或 allocator 采样。未实现的采样使用 `null`，不填 0。

后续完整验收仍需真实双机的 capture→queue→transport→assemble→decode→display 和 input→inject 关联；使用同一会话代数与帧号。不得直接减去两台机器的系统时钟；端到端用外部高速摄影/回显，或给出时钟校准误差。至少记录 Intel/AMD/NVIDIA/混合显卡、1080p/1440p、静态文字/滚动/视频、LAN/中继、带宽阶跃矩阵的 CPU/GPU/内存、功耗/温度、分配率、队列年龄、音画延迟和长稳斜率。当前切片不能关闭这些人工验收项，也不证明任何性能收益。

DL2-015 的 CPU 兜底现由每个编码器复用一个 NV12 Vec，1:1 转换直接计算字节偏移，移除每帧两份坐标数组；分辨率缩小时保留容量，所有有效 Y/UV 字节逐帧覆盖。原有 `convert_to_nv12` 仍返回新 Vec；需要复用时调用 `convert_to_nv12_into`。可在同一提交、相同输入下对照两条 API：

```powershell
python scripts/benchmark-windows-media.py --width 1920 --height 1080 --frames 120 --warmup 10 --output dist/windows/conversion-allocated.json
python scripts/benchmark-windows-media.py --width 1920 --height 1080 --frames 120 --warmup 10 --reuse-conversion --output dist/windows/conversion-reused.json
python scripts/run-windows-cargo.py test -p desklink-windows --test encoder_buffers --release --locked conversion_buffer_reuse_release_baseline -- --ignored --nocapture
```

`conversion_storage` 与 `known_conversion_buffer_grows` 只描述 NV12 输出 buffer 的显式容量增长，后者仅计测量帧（不含 warmup）。不是全进程分配率，也不包含 MF/驱动分配。`--encode` 总是使用生产编码器的复用路径；同样需要通过实际数据评估耗时，不能从 buffer 复用本身推断端到端收益。GPU 转色、surface 输入和硬件矩阵仍未关闭。

忽略的 release microbenchmark 在同一进程中对 1080p/1440p 各 warmup 5 次、测量 30 次，交替两条 API 的执行顺序；每帧同时断言输出逐字节一致和复用 pointer/capacity 稳定。它仅比较当前转换实现的分配/复用 API，不是旧提交的整条媒体链路对照。报告包含 ns 单位的 p50/p95/p99 和每条 API 的 NV12 写入字节数，不记录真实桌面内容。
