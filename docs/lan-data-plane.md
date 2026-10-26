# DL2-024：认证 LAN 多通道传输切片

本记录保留第一波 `desklink_transport::LanDataPlane` 组件及其验证。第二波已经通过 `SessionDataPlane` 接入 Windows Host、实际 ControllerRuntime 与 owned FFI worker；实现不定义另一份视频、音频、文件或输入格式。当前能力、唯一协议14来源和生产验证见 [生产接线记录](lan-runtime-integration.md)、[会话 transport 记录](lan-session-transport.md)。公网 NAT、LAN 发现/本地配对、真实双机延迟与长稳仍未验收。

下文的零上下文说明对应第一波冻结分支 `4b15561` 的历史过渡状态。第二波 `codex/dl2-lan-runtime` 已完整整合 Windows joint `5782341` 与 Apple `f954a68`，保留真实上下文生产、捕获传递及 Host 校验；本地整合验证见 [生产 LAN 接线记录](lan-runtime-integration.md)。LAN/codec 夹具本身仍只证明传输边界，不能据此声称生产 LAN、实机呈现或真实注入已通过。

## 第一波组件接线契约（历史）

1. 两端先完成现有 Noise 会话授权和 DirectLan probe。构造器只接受该 probe 返回的 `DirectLanConnection`，不能从任意未认证的 `quinn::Connection` 构造数据面。必须传入同一个 Noise 会话的 `Arc<Mutex<SecureSession>>`。
2. 两端在现有认证控制通道明确协商 DLD1 能力及切换边界后才能构造本数据面。第一波尚未增加该声明；第二波已复用唯一协议14来源并接通真实能力交集，不允许仅因 probe 成功便向旧 peer 发新帧头。一个连接只能交给一个 reader，本数据面不得与旧 `recv_datagram` 视频 reader 同时消费同一连接。
3. 切换后本对象负责所有八个通道的 seal/open；runtime 消费 `recv(channel)` 返回的已解密业务内容，不得再次 open。每个通道独立消费，不能在单一 select 分支中等待慢文件消费者而阻塞其他通道。
4. relay adapter 实现 `LanRelayBackend`：把收到的**相同密文**发送到现有、相同代数的对应 relay 通道，不能重新 seal。每个可靠通道保序。对端 relay reader 逐条 await `accept_relay_ciphertext`，再读取同通道下一条密文，不得绕过本数据面的统一解密和去重入口。adapter 要遵守会话代数，不能跨重连转发旧密文。
5. 输入通道中断、取消或总截止时间耗尽后，该可靠通道被标记不可继续。runtime 必须按既有安全生命周期 ReleaseAll 并重新建立授权会话，不能继续发送后续 keyUp，也不能重放被取消的 keyDown。调用 `close()` 会及时唤醒发送、lane-lock 等待与接收等待；对象销毁会关闭直连并 abort 自己的 reader 任务。
6. `LanRelayBackend` 仍由调用方持有，数据面关闭不隐式关闭共享 relay client。已建立的 LAN 路径可在 relay backend 不可用时继续工作，但这只在本地传输夹具证明，不等于现有生产 runtime 已能脱离 relay 维持会话。

## DLD1 传输信封

仅 LAN 使用该信封，relay 继续发送原有通道密文。多字节整数为网络字节序。

| 字段 | 字节 | 语义 |
| --- | --- | --- |
| magic | 4 | ASCII `DLD1` |
| version | 1 | `1` |
| channel | 1 | 原有 `ChannelKind` 的 1～8 |
| ciphertext length | 4 | 仅可靠双向流；范围 24～65535 |
| ciphertext | 可变 | 原有 `SecureSession` 对应 `SecureLane` 的 seal 输出 |

可靠消息的 AEAD 明文为 `DLB1`（4 字节）+ 随机 admission token（16 字节）+ 原业务明文；最大业务明文因此为 65491 字节。每条使用一个双向 QUIC 流，接收端在为该通道保留有界队列位置、完成 AEAD 验证并入队后，回送该条 16 字节 token 作为收讫，再结束响应流。只有能解密该条、该 lane、该 Noise 会话密文的端点才知道 token；透明转发 probe 的 QUIC 中间节点无法通过回送裸 `0`、密文本身、随机 token 或旧 token 冒充新消息的收讫。token 不使用反向业务 lane seal/open，因此大量收讫不会挤出反向业务消息的 128 包 replay window。

每个发送通道直到收到收讫或完成 relay 回退前只允许一个消息发送，从而避免跨流重排；不同通道不共享发送锁。Input/Control/VideoConfig/Transfer 的 QUIC stream priority 分别为 30/20/10/-10。收讫只证明传输队列接收，不证明远端已执行按键或完成文件落盘。

所有 LAN 业务内容仍受原有逐方向、逐 lane 的 AEAD 保护；channel 明文选择的密钥不同，改写 channel 不会产生合法业务内容，另一 Noise 会话的密文同样无法通过。信封不添加新的业务协议或密钥。datagram 总长 ≤1200，因此业务明文上限为 1170 字节（6 字节信封 + 24 字节原有密文开销）；超限在 seal 前拒绝，不消耗序号。现有 packetizer 以后接线时必须按新上限重新预算，不能把旧 1200 字节视频包直接塞入。

## 背压、去重与回退

可靠通道队列容量：Control 16、Input 32、VideoConfig 4、Transfer 4、VideoReliable 8。视频/光标/音频 datagram 队列分别为 8/2/8，满时丢弃新包，保持固定内存预算；没有帧依赖级淘汰。可靠 reader 最多同时 16 个，每条 reader 有总截止时间。QUIC 自身仍有共享拥塞窗口，priority 与独立队列不构成硬带宽预留或真实延迟指标。

LAN 可靠收讫默认最多等 250ms；整个 send（包括通道锁等待与 relay 回退）默认最多 2 秒。某 lane 的 LAN 失败只把该 lane 单向切到 relay，不影响其他 lane 的已选路径。不支持同一对象从 relay 自动返回 LAN，避免先前 relay backlog 被新 LAN 输入超越；重新启用必须建立新的协商边界。datagram 使用非阻塞 QUIC send，不在媒体网络等待中持有会话 crypto 锁。

LAN 收讫丢失或发送结果不确定时，relay 回退重用同一密文（含相同 DLB1/token）。接收端每个可靠 lane 保存最后一个实际成功入队的密文和 token，完全相同的重试只回送同一 token、不再入队；其他密文交给原有 AEAD/replay-window 判断。不能把所有 `ReplayRejected` 都当作已投递，因为它也可能指从未到达、但已落后超过 128 包窗口的消息。队列位置先 reserve、再 open，防止取消时只推进 crypto replay window 却未交付消息。旧 runtime 不认识 DLB1，relay reader 必须接入本模块剥除信封后才能消费业务内容。

## 第一波自动验证与当时剩余工作

本地 QUIC 夹具覆盖所有八个通道双向传输且 relay backend 离线、文件队列满时输入/音频独立、逐 lane 回退、LAN/relay 同密文只入队一次、错会话/错 lane 拒绝、未投递旧序号不被误判成功、关闭唤醒队列/接收、总超时阻止后续输入越过不确定消息、媒体上限及断开直连后的回退。

2026-10-26 本切片在 Windows 本地实际执行，`CARGO_TARGET_DIR=E:\desk-link\.worktrees\dl2-targets\network-lan`：

```text
python scripts/run-windows-cargo.py test -p desklink-transport --lib --tests --locked
# 35 unit + 16 localhost integration passed；新增 lan_data 测试 11 项
python scripts/run-windows-cargo.py clippy -p desklink-transport --all-targets --locked -- -D warnings
python scripts/run-windows-cargo.py fmt -p desklink-transport -- --check
git diff --check
```

伪收讫用例分别注入裸 `[0]`、错误 16 字节 token 和上一条成功消息的 token，三者均触发相同密文的 relay 重试。双向 replay 用例先堵住接收队列中的反向 Input，同时完成 130 条正向 Input/收讫，再验证先前反向消息仍正常送达。取消用例还在共享 crypto 锁持续占用时调用显式 `close()`，验证两个 reader 任务在 500ms 测试预算内回收。上述时间是用例截止预算，不是实机延迟或性能数据。

生产接线前仍需协商能力/代数与切换边界、适配现有 packetizer 和 relay readers、接通输入控制/音频/文件、定义 relay 不可达时的 LAN 存活及授权撤销、完善 LAN 发现和本地配对。能力协商应复用 DL2-009 统一的 v13 契约（来源独立提交 `9979c6424cf99f704feb212c7700833fce466dae`，本切片代码无其新增枚举依赖，后续由主会话整合并重跑验证），不单独修改全局 `PROTOCOL_VERSION` 或重排业务 `ControlMessage`。可靠逐消息收讫选择正确性优先，文件吞吐可能受单 lane 的收讫往返限制；需要本地及实机吞吐/帧龄/输入 p95/p99 测量后再设计有界滑动窗口，不能宣称该切片已达到极致性能。

## 共享边界跟进（2026-10-26）

已复用媒体组 DL2-011 独立提交 `3277c2a41af0ac923e1daa1c5cab5d952b355b9b` 和 `0836a8b311a858bf12f0d0b4427961c35f5986a7`。前者提供共享 crypto 的短锁探测 API，后者追加 `SecureLane::DirectProbe=8`，保持已有 0～7 不变，探测有独立的密钥、计数器和重放窗口。没有新增另一套 probe wire、重置相同 key 的计数器、变更协议 13 或 JOIN 信封。

`CHANNEL_COUNT=8` 是业务 `ChannelKind` 1～8 的数量，不是 `SecureLane::COUNT`；新增内部 probe lane 不增加业务队列或 QUIC event 槽。LAN 连接仍来自相同 probe API，本模块只追加 crate 内认证连接 getter。整合共享 API 时保留当前基线的静态捕获逻辑，仅接入 probe 调用与代数退出时的任务取消；没有把 LAN 数据面接入生产 Host，也未替换媒体组后续的输入快路径。

DL2-013 的 `InputEnvelope {stream_id,input_epoch}` 和 `VideoConfig.input_epoch` 由 Apple/FFI 提供唯一契约，本切片没有定义另一版本。业务正文在 seal 前仅包裹 DLB1/token，admit 后逐字节剥除包装并返回，不读取或重写输入字段。可靠业务上限仍为 65491 字节，epoch 占用业务体自身预算。Host 输入消费者必须校验 epoch；收讫仅表示传输入队，不能代替输入执行或 epoch 合法性。正式合同的传输回归见下节；Host 旧 epoch 拒绝和业务接线仍待 owner 完成。

以下是引入 DL2-013 合同之前的历史分支验证：crypto 36 项、transport 38 项单元 +16 项 localhost、relay 34 项常规、FFI 39 项以及 Windows `direct_video_tests` 2 项全部通过。它不能证明后续编译过渡合同下的 FFI/Windows 业务链路可用。手工性能 probe 本次未重跑，原记录仍对应此前测量基线；没有用此前延迟替代新接入的性能证据。该基线 crypto/transport/relay/FFI all-targets Clippy、Windows lib Clippy（均 `--locked -- -D warnings`）、五个 package 的 fmt 与 `git diff --check` 通过。

本地依赖提交分别为 `958f6262a42bb74e493eb82987f089516a93e5a7` 与 `84fe28952ebe2bf7aaf5e63ce44174aa41562f37`，均记录原始来源。整合时引入的 runtime 行尾变化在本跟进提交纠正，最终相对整合前的 runtime 差异只剩共享探测调用与任务取消管理。主分支若已有原始两个依赖，不应重复引入本地依赖副本。

## DL2-013 合同传输回归（2026-10-26）

已复用独立合同 `bc6c0a62d3817fbe44759cddf5069c3dee117f4c`（本地副本 `8417c2cfd78868ef44aab4b13607a20a3c30bbe6`），依赖同一协议 13 基线。只通过 protocol 的 `encode_input` / `decode_session_input` 处理输入；transport 不手工拼版本前缀，也不尝试无上下文 fallback。业务 codec 会拒绝零 context、旧布局、尾字节与错版本。

新增两个真实 LAN QUIC 回归。Host 夹具使用 `encode_video_config` 公布有效 stream/epoch，Controller 通过收到的 `decode_video_config` 结果捕获输入上下文，再调用 `encode_input`。覆盖最小非零值、`u64::MAX` 及组合边界、sequence/timestamp 边界，验证输入 wire（含开头零字节）完整保留。缺少收讫时观察到的 LAN 密文与 relay retry 逐字节相同，统一 ingress 解密后 `decode_session_input` 得到原上下文，相同 retry 不重复入队。夹具不会把发送时的“当前 epoch”覆盖到已捕获输入上。

本分支实际通过：protocol 47 项；transport 43 项单元 +16 项 localhost（LAN 数据面现 13 项）；opaque relay 34 项常规。protocol/transport all-targets Clippy（`--locked -- -D warnings`）、transport fmt 和 `git diff --check` 通过。命令仍使用仓库 `scripts/run-windows-cargo.py` 和独立 target 目录。

第一波当时未验证/未交付：该过渡合同的 `HostVideoPipeline`、FFI host worker epoch 与 ControllerRuntime/FFI emit_input context 曾为零占位，正式 codec 会拒绝。第二波已完整合入 owner 的实际上下文来源，不补默认 `1`；上文历史测试并未沿用作新生产证明。当前的实际验证、未运行平台及 DL2-024 主项剩余边界见生产接线记录。
