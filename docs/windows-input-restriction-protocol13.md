# 输入受限反馈协议 13（DL2-009 契约切片）

`crates/desklink-protocol/src/lib.rs` 的唯一 `PROTOCOL_VERSION` 由 12 升至 13。目录注册、目录查询、Noise 握手及媒体头均继续引用该常量；协议版本不匹配由现有目录和握手边界拒绝。开发客户端需要使用同一协议代数，不增加旧协议互通层。

`ControlMessage` 尾部新增 `InputState { reason: Option<InputRestrictionReason> }`，Postcard variant tag 为 14，现有 tag 0～13 保持不变。

| reason | 含义 |
| --- | --- |
| `Some(Blocked)` / JSON `blocked` | 原生输入注入受系统或桌面权限限制 |
| `Some(UnsupportedKey)` / JSON `unsupportedKey` | 当前输入键或按钮不受支持 |
| `Some(InvalidInput)` / JSON `invalidInput` | 输入值未通过原生边界校验 |
| `None` / JSON `null` | 先前受限的输入重新成功注入 |

原因使用固定枚举，不将系统错误文字作为协议或状态判断依据。该消息只报告输入状态，不改变会话、画面或重连生命周期。

本提交只建立共享契约与协商阶段的完整匹配适配，不声称 Host 已发送反馈或前端已展示提示。持续失败汇总、恢复判断、Controller signal 及权限场景真机验收仍由 DL2-009 的后续切片完成。

回归覆盖新消息的全部原因和恢复序列化、旧消息 wire tag 不偏移、统一协议常量，以及共享协议 / FFI 本地自动检查；Windows 管理员窗口、UAC、安全桌面和锁屏的可控性不能由这些合成结果推定。

直接/保存连接收到协议 12 的 Noise 回复时会终态拒绝；旧端若静默丢弃新版本握手，则只有超时，不能据此声称已识别版本。该静默路径的重试预算与可操作提示保留为后续验收，目录不匹配拒绝也不能替代它。

实际本地验证（`CARGO_TARGET_DIR=E:/desk-link/.worktrees/dl2-targets/windows-product`，均经 `python scripts/run-windows-cargo.py`）：

- `test -p desklink-protocol -p desklink-ffi --lib --tests --locked`：77 项通过。
- `test -p desklink-relay --test session directory --locked`：6 项通过，包括 12 ↔ 13 双向版本拒绝与错误密码优先隐藏版本信息。
- `test -p desklink-windows-ui --lib protocol_decode_failure_stops_retrying_with_an_upgrade_action --locked`：1 项通过，收到旧 Noise 版本后提供终态升级提示。

直连 / 已保存连接测试直接加入本地 relay 会话，绕过目录，验证 Controller 收到协议 12 的 `ResponderHello` 时立即返回 `UnsupportedVersion(12)`。这不证明旧 Windows 12 Host 会回应协议 13 的主动握手：旧实现可能把版本错误当作过期消息静默丢弃。新 Host 的明确版本拒绝响应，以及控制端针对无回应握手的有界恢复策略，仍需后续独立验收，不能以这项测试宣称已消除全部旧端重试路径。
