# 协议 14 LAN 数据面协商合同（来源切片）

本切片只定义共享业务合同和 codec 校验，是供并行实现引用的来源提交和非整链过渡。Windows、macOS、iOS 集成时的生产能力均为 `lan_data: None`，尚未接通 LAN 数据信封、切换状态机或 relay 失联后的业务延续；不能据此声明这些功能可用。各运行时的 None 字段补齐和明确拒绝未经协商切换的接线由后续整合提交收录。

`DeviceCapabilities` 末尾追加 `lan_data: Option<LanDataCapabilities>`。非空值包含 `envelope_version: u16`（仅允许 1）、`channels: u16`（非空且仅包含下表已知位）和 `allow_relay_loss_continuation: bool`。缺少可用能力或任一端为 `None` 都不启用 LAN 数据信封。Postcard wire 必须显式编码完整 Option；截断字段不是能力缺省，解码直接拒绝。无效版本和位掩码在 encode、decode 两端均拒绝。

| 位 | 既有 ChannelKind ID | 通道 |
|---|---:|---|
| 0 | 1 | Control |
| 1 | 2 | Input |
| 2 | 3 | VideoConfig |
| 3 | 4 | VideoDatagram |
| 4 | 5 | CursorDatagram |
| 5 | 6 | Transfer |
| 6 | 7 | AudioDatagram |
| 7 | 8 | VideoReliable |

两端完成 Noise 身份认证后，在生产业务消息开始前交换并验证能力。协商取双方通道交集，延续标记取逻辑与；无交集则不启用。运行时必须冻结此次会话的信封模式，relay 和 direct 使用同一种模式，从首条生产业务消息起生效，不能在业务已开始后切换消息格式。现有输入上下文、Noise 加密绑定和 relay JOIN 信封不变。

`ControlMessage` 在原有变体之后追加：

```rust
LanDataSwitch {
    candidate_id: u64,
    generation: u64,
    channels: u16,
    phase: LanDataSwitchPhase, // Offer / Ready / Commit
}
```

candidate ID 和 generation 必须非零，channels 使用相同的非空已知位规则，仍受现有 control 长度上限和精确消费边界约束。Codec 只校验语法；运行时必须核对已经认证且未过期的候选及其现有 Noise session binding，限制 channels 为已冻结协商的子集，并对完全相同的 candidate/generation/channels 执行 Offer → Ready → Commit。旧 generation、乱序、重复提交和未经协商的请求必须拒绝。`allow_relay_loss_continuation` 只有双方为真且切换已提交、真实 direct 通道仍有效时才可使用，具体连接存活、回退和资源清理由 LAN 实现会话完成。

协议业务版本保持在统一的 14 开发线上，旧 13 端按现有握手版本边界拒绝。新增控制变体采用追加方式保留既有 discriminant。此切片没有更改 JOIN、输入序列和任何加密算法。

自动验证覆盖：能力 None/Some、全部已知通道、非法版本/空掩码/未知位、协商交集、三阶段完整 u64 round trip、既有变体位置、新消息截断/尾随/长度上限拒绝。真实 LAN 切换、relay 失联续用、双机和 Mac 仍需后续运行时与设备验证。

本地执行 `python scripts/run-windows-cargo.py test -p desklink-protocol`：新增 LAN 测试 7/7 通过；完整协议测试 63 通过，另有既有离线 CPU benchmark 1 项 ignored。没有以 ignored 项建立任何功能或性能结论。
