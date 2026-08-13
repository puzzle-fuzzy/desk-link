# Windows 4K 编码评估

DeskLink 当前正式发布边界仍是最高 `2560×1440`。4K 不是默认能力，也不会因为控制端支持 High Profile 就自动打开；这样可以避免公网中继、普通核显和低带宽网络被 4K 码率拖入持续重连。

## 当前代码结论

- `H264EncoderSettings::experimental_4k()` 是显式实验配置：`3840×2160`、30 FPS、40.5 Mbps、H.264 High Profile。
- 默认发布配置仍限制为 `2560×1440`，默认码率为 18 Mbps。
- Windows 控制端支持 High Profile 时，主机在能力协商阶段优先使用 High；Media Foundation 不支持时自动回退到 Main，并保留实际 profile 供诊断读取。
- DirectLan 质量门槛只表示链路具备实验条件，不会单独放开 4K；真正启用 4K 仍需要编码器、捕获、解码和长稳验收全部通过。

## 真实探针

在 Windows 交互桌面上运行：

```powershell
cargo test -p desklink-windows --test encoder_smoke experimental_4k_media_foundation_captures_and_encodes -- --ignored --nocapture
```

这个探针会实际捕获当前主屏，初始化 4K Media Foundation 编码器，编码最多 10 个访问单元，并验证首个访问单元包含关键帧和解码配置。它是能力探针，不会修改应用默认设置，也不会把 4K 标记为已发布能力。

## 放开 4K 前的门槛

1. 真实 Windows 设备完成 4K 捕获/编码/解码，确认首帧、关键帧请求和双屏切换正常。
2. 同网 DirectLan 与公网中继分别记录 30 分钟的帧率、编码耗时、GPU/CPU、内存、丢包和恢复次数。
3. 4K 仅允许在 DirectLan 质量门槛通过且控制端明确选择时启用；中继保持 2560×1440 上限。
4. 所有受支持的 Windows 编解码器/显卡组合都能在失败时回退到 2560×1440，而不是让主机服务停止。

在以上证据齐全前，发布说明继续使用“最高 2560×1440”的产品承诺。
