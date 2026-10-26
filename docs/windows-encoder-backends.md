# Windows H.264 后端选择与降级（DL2-016）

初始化先通过 `MFTEnumEx` 的 hardware + sort/filter 标志按 NV12 输入/H.264 输出枚举候选，最多尝试 16 个激活对象。每个候选必须声明异步处理、允许 async unlock、支持请求的输入/输出类型和流启动；驱动拒绝配置时尝试下一候选，全部失败则显式创建 Microsoft 软件 MFT。High 不可用时沿既有契约回退 Main。

硬件 MFT 通过 `IMFMediaEventGenerator::GetEvent(NO_WAIT)` 获取事件；每个 NeedInput/HaveOutput 只授权一次对应 ProcessInput/ProcessOutput，事件信用有固定上限。输入加本帧输出的事件等待预算为 100ms；没有进展、错误事件、无效输出或事件洪泛进入软件降级。同一输入帧在新 config_version 中以 IDR 重试，清除旧引用帧队列；本编码器实例后续重配继续使用软件，防止重复硬件故障来回振荡。该预算限制事件等待，不声称能中断阻塞在驱动内部的 COM 调用。

`H264Encoder::backend_info()` 提供实际后端类别、驱动名称、候选/拒绝数量和稳定降级原因；`profile()`/`dimensions()` 仍是实际配置。离线基线报告也包含该信息，未激活编码时为 null。硬件路径当前使用 **CPU NV12 system-memory 输入**，仍有 CPU 转色和 MF buffer 复制；没有把它描述为 GPU surface 或零复制链路，DL2-015 的 GPU 常驻验收继续未完成。

使用同机、同配置、相同静态/运动合成输入对照：

```powershell
python scripts/benchmark-windows-media.py --encode --software --width 1920 --height 1080 --frames 120 --output dist/windows/software-1080p.json
python scripts/benchmark-windows-media.py --encode --width 1920 --height 1080 --frames 120 --output dist/windows/prefer-hardware-1080p.json
```

硬件报告可能实际选择软件；应先读取 `backend.kind` 和 `fallback_reason`，不能把命令参数当作硬件已启用的证据。当前自动回归使用软件 MFT 加合成故障验证 config/IDR/同帧重试，不捕获桌面、不打开远控会话；Intel/AMD/NVIDIA、混合 GPU、D3D surface、设备丢失、驱动参数差异和性能/功耗矩阵仍须真实设备运行。

依据：[MFTEnumEx 的类别与资源释放](https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mftenumex)、[异步 MFT 的事件与 unlock 契约](https://learn.microsoft.com/en-us/windows/win32/medfound/asynchronous-mfts)。
