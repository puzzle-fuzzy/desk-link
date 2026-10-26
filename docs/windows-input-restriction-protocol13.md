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

共享契约切片建立协商阶段的完整匹配适配；后续输入反馈切片接通 Host 汇总、恢复、Controller signal 和状态提示。真实权限场景仍需设备验收，不能用本地合成结果替代。

回归覆盖新消息的全部原因和恢复序列化、旧消息 wire tag 不偏移、统一协议常量，以及共享协议 / FFI 本地自动检查；Windows 管理员窗口、UAC、安全桌面和锁屏的可控性不能由这些合成结果推定。

直接/保存连接收到协议 12 的 Noise 回复时会终态拒绝；旧端若静默丢弃新版本握手，则只有超时，不能据此声称已识别版本。Windows 对已建立 peer 但安全握手连续三次未响应设置独立退出预算；普通离线不消耗此预算，认证成功后重置。提示检查目标在线、网络与两端版本；FFI 保留现有最多六次重试。目录拒绝不能替代直接握手边界，协议 13 Host 主动回复旧版 InitiatorHello 的分支仍待后续闭环。

## Windows 输入权限与恢复边界

当前 Windows manifest 为 `asInvoker`、`uiAccess=false`。持续输入失败按指针、键盘、按钮、滚轮分别累计：同类连续三次相同原因后报告；释放事件和零滚轮不作为恢复依据，另一类成功也不清除原类限制。实际同类注入成功后消除该类限制，全部恢复才发送 None。输入告警只修改状态提示，画面与会话继续保留。

| 场景 | 提示与下一步 | 本次证据 |
| --- | --- | --- |
| 普通桌面 | 正常输入；失败按稳定原因提示并在真实恢复后清除 | 合成输入后端与状态机测试；双机待验 |
| 管理员窗口 | 输入受限时请被控用户切回普通桌面或本机处理 | 当前权限声明已查；真实权限注入待验 |
| UAC / 安全桌面 | 请被控用户在本机确认，保留当前画面连接 | 无真实 UAC / 安全桌面控制结果 |
| 锁屏 | 请被控用户本机解锁后再尝试输入 | 无真实锁屏控制结果 |
| 不支持的键 | 尝试快捷键菜单或在被控电脑本机完成 | 原生 UnsupportedKey 分类与合成 UI 信号测试 |
| 非法输入样本 | 短暂坏样本不显示持续权限告警；持续发生时提示重新点击画面 | InvalidInput 独立分类，单次失败和恢复测试 |

此流程不自动请求提权、不重启会话，也不把输入受限推断为网络断线。实际平台权限覆盖需要对应设备与授权条件单独验收。

实际本地验证（`CARGO_TARGET_DIR=E:/desk-link/.worktrees/dl2-targets/windows-product`，均经 `python scripts/run-windows-cargo.py`）：

- `test -p desklink-protocol -p desklink-ffi --lib --tests --locked`：77 项通过。
- `test -p desklink-relay --test session directory --locked`：6 项通过，包括 12 ↔ 13 双向版本拒绝与错误密码优先隐藏版本信息。
- `test -p desklink-windows-ui --lib protocol_decode_failure_stops_retrying_with_an_upgrade_action --locked`：1 项通过，收到旧 Noise 版本后提供终态升级提示。

直连 / 已保存连接测试直接加入本地 relay 会话，绕过目录，验证 Controller 收到协议 12 的 `ResponderHello` 时立即返回 `UnsupportedVersion(12)`。这不证明旧 Windows 12 Host 会回应协议 13 的主动握手：旧实现可能把版本错误当作过期消息静默丢弃。新 Host 的明确版本拒绝响应，以及控制端针对无回应握手的有界恢复策略，仍需后续独立验收，不能以这项测试宣称已消除全部旧端重试路径。
