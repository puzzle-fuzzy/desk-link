# Windows H.264 后端选择与降级（DL2-016）

初始化先通过 `MFTEnumEx` 的 hardware + sort/filter 标志按 NV12 输入/H.264 输出枚举候选，最多尝试 16 个激活对象。每个候选必须声明异步处理、允许 async unlock、支持请求的输入/输出类型和流启动；驱动拒绝配置时尝试下一候选，全部失败则显式创建 Microsoft 软件 MFT。High 不可用时沿既有契约回退 Main。

硬件 MFT 通过 `IMFMediaEventGenerator::GetEvent(NO_WAIT)` 获取事件；每个 NeedInput/HaveOutput 只授权一次对应 ProcessInput/ProcessOutput，事件信用上限为 128。调度不假定每提交一帧马上输出：MFT 再次请求输入时，已提交的帧保留 metadata 并返回等待更多输入；较早输出先被读取并保留原始时间戳，本次新输入也必须提交成功后才返回较早输出。每个输出 sample 独立保留，INCOMPLETE 标记不用于合并不同时间戳的帧。

单次无事件等待预算为 100ms，跨调用的持续无输出 watchdog 为 1 秒，未交付输入/输出最多 16 帧、已读输出最多 8MiB；不断请求输入不会重置 watchdog。超过预算、错误事件、无效输出或事件洪泛进入软件降级，metadata 不再通过淘汰最早帧来悄悄丢失待输出 IDR。同一输入帧在新 config_version 中以 IDR 重试，清除旧引用帧队列；本编码器实例后续重配继续使用软件，防止重复硬件故障来回振荡。预算限制事件等待，不声称能中断阻塞在驱动内部的 COM 调用。

`H264Encoder::backend_info()` 提供实际后端类别、驱动名称、候选/拒绝数量和稳定降级原因；`profile()`/`dimensions()` 仍是实际配置。离线基线报告也包含该信息，未激活编码时为 null。硬件路径当前使用 **CPU NV12 system-memory 输入**，仍有 CPU 转色和 MF buffer 复制；没有把它描述为 GPU surface 或零复制链路，DL2-015 的 GPU 常驻验收继续未完成。

使用同机、同配置、相同静态/运动合成输入对照：

```powershell
python scripts/benchmark-windows-media.py --encode --software --width 1920 --height 1080 --frames 120 --output dist/windows/software-1080p.json
python scripts/benchmark-windows-media.py --encode --width 1920 --height 1080 --frames 120 --output dist/windows/prefer-hardware-1080p.json
```

硬件报告可能实际选择软件；应先读取 `backend.kind` 和 `fallback_reason`，不能把命令参数当作硬件已启用的证据。当前自动回归使用软件 MFT 加合成故障验证 config/IDR/同帧重试，不捕获桌面、不打开远控会话；Intel/AMD/NVIDIA、混合 GPU、D3D surface、设备丢失、驱动参数差异和性能/功耗矩阵仍须真实设备运行。

依据：[MFTEnumEx 的类别与资源释放](https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mftenumex)、[异步 MFT 的事件与 unlock 契约](https://learn.microsoft.com/en-us/windows/win32/medfound/asynchronous-mfts)。

事件顺序回归覆盖 NeedInput→输入1→NeedInput→输入2→HaveOutput（输入1时间戳），以及先到输出、新输入不丢失、持续无输出、输入/事件洪泛和等待截止边界；使用可注入单调时间验证有界性，没有把这些夹具当作真实硬件编码验收。[INCOMPLETE 的官方语义](https://learn.microsoft.com/en-us/windows/win32/api/mftransform/ne-mftransform-_mft_output_data_buffer_flags)说明仍可产生更多输出。

实际产生输出后，时间戳必须属于本次输入或某个待输出输入。未知时间戳返回稳定的 `encoder_output_timestamp_unmatched` 错误，不再当作尚无输出的 NeedMoreInput，也不消耗其他帧的 metadata。硬件缺失时间戳使用 `encoder_output_timestamp_missing`；两者均进入现有同帧软件重试、新 config_version/IDR 和 SoftwareOnly 锁定，避免恢复时重新选择相同的异常硬件。仅软件后端保留无时间戳时的旧 FIFO 语义；软件返回未知的明确时间戳仍产生可恢复错误，交由既有重建路径处理。自动回归通过软件 MFT 加输出时间戳故障注入验证这些分支，不代表真实硬件异常已经实测。

DL2-023 的 `H264Encoder::try_set_bitrate` 单独调整 ICodecAPI MeanBitRate/MaxBitRate。两项均必须可修改且能读取 uint32 值；升码率先升峰值，降码率先降均值，写入后精确回读才更新 `bitrate()`。拒绝、部分写入、驱动钳制或回读失败后尝试还原两项，并再次验证。`Ok(false)` 表示未写入的不支持，或已经验证恢复原值；无法确认回滚时返回错误，调用方必须用旧设置重建并请求 IDR。成功不会重建编码器、增加 config_version、清空参考队列或改变尺寸/帧率/profile。

自动测试用可注入属性模拟部分写入、钳制、回读失败及回滚失败，并用本机软件 MFT 的 64×64 合成帧验证拒绝后的输出与代数连续。本轮软件 MFT 返回 `accepted=false`，因此没有把它记为真实动态调码率成功；硬件设备上成功调整及其带宽/质量收益仍未验证。
