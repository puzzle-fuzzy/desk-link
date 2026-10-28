# DeskLink 第二阶段 TODO

> 更新日期：2026-10-26（Asia/Shanghai）
> 全分支整合：第一波、GPU、文件任务、LAN 运行时及五个依赖升级分支已保留完整历史并进入同一协议14候选；本次合并与验证范围见 [全分支整合记录](docs/all-branches-integration.md)。下文“尚未合入 main”及旧 main 版本均为各来源冻结时的历史快照，不代表整合后的主分支状态；未完成的设备、平台与性能验收继续保留。
> 初始审查基线（历史）：`main@85bdf7278baf11b1adb9b401b978643c261daf2b`；Windows 客户端 `0.1.93`，协议 `12`。当前已提交成果的源码身份与子项检查框见下方快照；分支成果尚未合入 main。
> 当前第一波软件验证源：`a4c5b404d43794d646394923a481f23c9298c3ed`（执行时干净，协议13），25个检查全部通过；平台报告仍 incomplete（2通过/7跳过）。完整来源、原始报告及本地签名→HTTP/SQLite证据见 [第一波集成记录](docs/wave1-integration.md)。本文后续文档收口不改写该测试源；main仍未合入。
> 当前第二波GPU验证源：`97ff7c08199cdad4511b0044ddf783d79e2295b1`，tree `2319d0e73bb68c22496d43ec1fb99f84f4e1dd6f`（执行时干净）；25软件检查全通过，6条选中GPU命令共24通过/0忽略，新native签名→本地HTTP/SQLite19案例全通过。平台仍2通过/7跳过、incomplete；真捕获/恢复/同画质性能未验。见[第二波GPU集成记录](docs/wave2-gpu-integration.md)；第一波V/a4与冻结c3记录保留原来源。
> 本阶段目标：补齐远控工作流、消除真实缺陷、重整模块职责，并以可重复测量推进低延迟、低资源占用和稳定吞吐。允许调整协议、ABI、模块边界及实现，不以开发成本或现有部署兼容性限制方案。
> 用户已确认 Windows → Windows、macOS → Windows 可以正常连接并良好控制，这两条路径是已成立的产品基础。本文件不把旧文档中“尚未验收”的历史表述当作当前不可用证据。
> 本清单已进入并行实施阶段。各项按下述会话分工推进，完成一个问题并通过对应验证后立即独立 commit，同时更新该项完成标记。第一阶段记录保留在文末，不能用其旧日期、版本或勾选状态代替本阶段验收。
> 2026-10-26 复核：草稿引用的 30 处 file:line 证据已逐条读源码重验，全部成立；另经两个独立专项审查（服务端、共享 crates）新增 DL2-044～061 共 18 项经源码逐条抽查确认的问题，其中 DL2-052 判定为 P0。所有新条目证据均已在主 checkout 上以源码核实。

## 1. 如何使用这份清单

证据分为四类：

- **复现缺陷**：本次使用真实模块和本地合成输入复现，不涉及线上服务或真实用户数据。
- **源码缺陷**：沿真实调用链确认存在错误分支、契约冲突或生命周期问题，尚未做对应双机复现。
- **能力缺口**：代码明确没有接通或存在硬性限制；属于需要补齐的功能，不代表基础远控不可用。
- **性能结构**：源码证明存在复制、分配、锁等待或调度开销；收益幅度、当前 CPU 占用及用户感知延迟必须实测。

优先级：**P0** 为公网可达、利用成本低且影响全部会话的缺陷（当前仅 DL2-052，2026-10-26 复核新增）；**P1** 为影响输入、恢复、控制权、机密性或核心性能的优先工作；**P2** 为完整体验、持续性能及工程收口；**P3** 为扩展角色。首轮审查未判定 P0。

每项关闭时必须补充修复提交、相关测试与真实设备结果。涉及硬件、权限、网络、音频和安装的条目，单元测试或构建通过只关闭对应自动验证部分。实测目标是未来验收标准，不是当前产品测量结果。

| 工作面 | 任务 | 主要结果 |
| --- | --- | --- |
| 核心正确性、输入与恢复 | DL2-001～013、044～048 | 输入完整、取消有界、重连不中断、主机可立即收回控制、会话密钥双方贡献 |
| 性能与数据通道 | DL2-014～024、049～050 | 可测量的媒体链路、减少复制、隔离慢路径、低延迟调度、传输栈收口 |
| 功能完备与用户流程 | DL2-025～035、051 | Mac 功能接通、设备可辨识、文件与账号操作闭环 |
| 服务端与长期运行 | DL2-036～039、052～058 | 并发注册正确、健康状态可信、存储与邮件可持续运行、公网资源上界与停机语义 |
| 代码整理与验证 | DL2-040～042 | 统一契约、拆解职责、自动验证覆盖真实边界 |
| 扩展角色 | DL2-043、059～061 | 被控 Mac 的能力协商、事件契约、输入新鲜度与直连超时一致 |

### 并行分工与提交规则（2026-10-26）

| 会话方向 | 负责条目 | 分支 | 首批执行 |
| --- | --- | --- | --- |
| Windows 产品与交互 | 001、002、008、009、017、031、032、033、034、035 | `codex/dl2-windows-product` | 001、034、032，再处理会话生命周期和共享控制 |
| Apple 与 FFI | 003、004、005、006、007、018、025、026、027、028、029、043 | `codex/dl2-apple-ffi` | 003、005、029，再完善 ABI、输入法和会话能力 |
| Windows 媒体 / GPU 第二波 | 015、016 | `codex/dl2-windows-gpu` | 原媒体65c冻结交接；后续GPU surface/硬件编码单独合入 |
| 中继与直连 | 第一波019/052/053/057冻结；第二波024 | 第一波 `codex/dl2-network`；后续 `codex/dl2-lan-runtime` | 4b15561可进入第一波；024真实生产接线后置 |
| 账号与诊断服务 | 036、037、038、039 | `codex/dl2-services` | 036、037、039，再处理邮件与长期运行 |
| 工程验证与收口 | 040、041、042 | `codex/dl2-engineering` | 040共享transport策略已落生产；041第一波软件收口，平台/设备继续待验 |
| 大文件与目录传输（第七会话） | 030 | `codex/dl2-file-transfer` | 从Windows joint5782341起步，独占新增文件功能的业务协议14 |

原媒体第一波010/011/012/013/014/020/021/022/023及030既有切片冻结在 `65c70fd658d0526a8a27b75fdbf120ae065222bf`，已进入Windows joint；新GPU与文件功能继续独立提交。文件会话为 `01a17733-4bf0-79d3-831f-7c0190191591`，负责新增文件wire/API的协议14；当前工程与Windows joint仍协议13，main仍12。

第一波独立候选已完整merge冻结Windows joint5782341、Apple f954a68及receipt后继cb751、网络4b15561、服务最终d2d4、工程6b31360、GUI506及原生诊断producer28c11，保留来源历史。干净a4c5b40已实际联合验证25检查全通过；main合入仍由主会话决定，工程组不切换或覆盖root checkout。GPU235的015/016、新文件030/协议14、024后续生产接线继续独立，不计入本第一波测试源。

第二波在第一波冻结 `c3fa4df987fb88be08eb5aeea9c7983b556e87d3` 后另建独立worktree，正常merge GPU235完整七笔历史形成97ff；其两个parents确为c3fa4df与23526f23。GPU现已在此独立候选实际联合验证，仍不回写为第一波V包含；文件030/协议14、024后续生产接线另行集成。

网络第一波已冻结 `4b15561586fab7a1481ba6dd4ff4854f34123018`（直接parent为a41e1e2），后续024在独立LAN runtime分支推进。**历史集成前快照：** 服务S快照后发现resetPassword预算满先消费token、token动作共用email:missing桶两处确定缺口，当时等待owner补修；当前最终 `d2d4f1c4c4942df03eda71e8aa1ebf3f8ab22c3a` 已包含补修并进入a4软件验证，部署/长期运行仍待验。

044/045服务schema历史来源分别为 `b1118ad465703493255123e23b6f522662c3d603` / `bf5d7eb27f3f41be59ac317b63d7e93d10f38066`；GUI506、原生producer `28c11ee05a08183f7636ede7946e93029c7bbb16` 与服务最终d2d4已完整merge。a4新native签名fixture→本地HTTP验签/schema→临时SQLite重开/去重/分析共19案例全通过，关闭全u64本地合同切片；设备GUI和远端生产送达仍待验。历史未提交cloud过滤切片已全部撤回，正常上传策略保留；撤回前中间编译日志不作为当前源码证据，observer仍只计尝试/发送错误，不计真实丢包。

- 各会话在独立 worktree 中工作，不在主 checkout 切换分支或提交别人的改动。表中编号均为 DL2 编号。
- **一个问题一个 commit。** 对应实现、必要测试和该项 TODO 状态一起提交，主题包含 DL2-ID；验证通过就提交，不累计多个问题。整体重构中可独立验证的切片也必须标明所属问题。
- **已完成用 `[x]`，未完成用 `[ ]`。** 主任务只有满足其适用验收要求后才勾选；代码、自动验证、真机验收分开记录，允许只勾选已完成部分。缺少平台、设备或性能数据时，已验证代码仍可提交，但主任务保留未完成并注明缺项。
- 每个问题至少补充修复内容、实际验证命令/结果、未验证条件和提交主题；精确 SHA 在会话交付或后续汇总记录中填写，避免提交正文自引用自身 SHA。
- 先做现有接口内的修复；DL2-004/011 涉及 transport，DL2-009 涉及 Windows runtime，DL2-024/025/030 涉及协议/ABI，变更前检查其他分支已提交的相关修改。先完成接口边界，再开展依赖功能；不并行实施相互替代的协议方案。
- 各会话只维护自己编号的 TODO 条目及必要证据，不改其他任务状态。DL2-040 不提前跨组重写，集成时逐问题保留提交；不把仅在分支完成当成已经合入 main。
- 仅处理本地代码、测试及提交；线上部署、发布和真实远控/输入注入不作为本次安排的自动动作。

**复核新增条目的分工建议（2026-10-26）：** DL2-044～046 归 Windows 媒体与文件；047 归 Windows 产品与交互；048 涉及 Noise 握手协议，须与 DL2-024/025 的协议变更统筹后由单一会话实施；049、052～053 归中继与直连；050 归 Windows 媒体与文件（desklink-video）；051、059～061 归 Apple 与 FFI；054～058 归账号与诊断服务。其中 DL2-052 为 P0 配置级加固，任一会话可立即优先处理，不与客户端分支冲突。

### 已提交成果检查框历史快照（2026-10-26；来源冻结时尚未合入main）

下表的 `[x]` 只关闭其后明确列出的代码或自动验证切片，剩余验收继续 `[ ]`。来源均为不可变Git提交；V保留第一波实际联合验证，G另列当前第二波独立候选实跑，其他代号保留来源/历史报告身份，不能跨范围累加或将来源总数当候选结果。main只读核对仍为 `a905b8807c83677f6f962c18f8fd1593f0d4ca32`，下列来源尚未成为main祖先。

| 来源代号 | 不可变source / joint | 证据范围 |
| --- | --- | --- |
| V（当前第一波） | `a4c5b404d43794d646394923a481f23c9298c3ed`，tree `154d51e9258a7731d78e78e1d7462bba475c0e36` | 干净源fast/integration 25检查全pass；Bun前端474/账号79/诊断129、Python257、FFI91、Windows292/17忽略、relay64/3忽略、Windows UI141/1忽略；native 2通过/7跳过仍incomplete；新native签名→本地HTTP/SQLite19案例全过。计数范围有重叠，不求和；见[完整报告](docs/wave1-integration.md) |
| J | `5782341e22ba55b2efe30a8eeaeafbebfe0daaba`，tree `59c7270aea76bc1d542ac49c625e308b42ef20f2` | Windows产品27fec+媒体65c真实合并；Rust634通过/2失败/19忽略，Bun474通过及构建、八包严格Clippy通过。两失败为未合完整Apple链的Host零epoch，Rust整体exit101 |
| M | `65c70fd658d0526a8a27b75fdbf120ae065222bf` | 冻结媒体448通过/18忽略、37笔提交；已成为J祖先 |
| A / CB | `f954a68153168a1ded048ddf93eb9c565684d096`，tree `eed9ea733f256024f5839ce0a7c6763d3a6a920a`；后继 `cb7514cc29b7a94ad40ac00f0a5b98936b7d5be5` | Apple协议13完整41笔历史及input receipt/admission后继均已merge V；原生165/0（FFI88+transport/relay77）、Python201为A来源历史报告，当前以V实跑为准；6项Swift Recording与Xcode/真实拓扑未运行 |
| N | `4b15561586fab7a1481ba6dd4ff4854f34123018` | 网络第一波冻结，继承a41e1e2；本地QUIC/调度/资源/deadline及Linux候选关停证据，生产部署/真实WAN另验 |
| S | 最终 `d2d4f1c4c4942df03eda71e8aa1ebf3f8ab22c3a`；历史快照 `b1118ad465703493255123e23b6f522662c3d603` | 最终schema/预算/分桶/u64历史完整merge V，本地数据库/Bun/进程回归和19案例签名/store联合链已执行；旧快照后两处补修等待已结束，未部署 |
| E | 最终 `6b31360ce4dc40d0f1715ce1e40c000788a6b812`；历史快照 `12b54ceedc0799a556e502436bc08830db7edca1` | 工程入口/合同及单一transport恢复分类完整merge V；29/18/12及三包严格Clippy/fmt为原策略切片报告，当前另有V整套25检查；其他历史报告见 `docs/local-verification.md` |
| G（当前第二波独立候选） | 实测 `97ff7c08199cdad4511b0044ddf783d79e2295b1`，tree `2319d0e73bb68c22496d43ec1fb99f84f4e1dd6f`；GPU来源 `23526f23f450e64edea098a36b07ed4c62d4982b` 完整七笔 | 干净源25软件检查全pass：Windows297/32默认忽略、workspace516/18忽略、FFI91/0、relay64/3、Bun474/79/129、Python257；6条选中GPU命令8/6/4/1/1/4共24通过/0忽略，含15项原ignored（含inventory），5项lib默认用例与软件套件重叠，另4项example，不跨范围求和。新签名HTTP/SQLite19通过；platform2通过/7跳过仍incomplete。GPU290/17、早期2d79的204/6与7GPU仅保留owner历史来源，均不改写V；见[第二波报告](docs/wave2-gpu-integration.md) |
| D / producer | `5069158584dfa1f729defe2fadb5b9933acdfcff`，parent67cd1bf、tree `3740f354bce3c1d14d32f8435bb44b0c2b6a1006`；后继 `28c11ee05a08183f7636ede7946e93029c7bbb16` | GUI044/045及完整u64 producer已merge V；Windows218/2、UI137/1与138/1及限定Clippy为D来源历史报告。V实际验证规范十进制string/原始签名字节与服务store/analysis全值域链；设备GUI/远端生产送达未验 |

来源TODO已关闭的主项为：产品001/034/035，媒体020/046/050，服务036/037/038/039/054/055/058，工程042；这些主项的来源身份保留，当前第一波软件集成与服务补修已由V验证，平台、设备、部署及性能剩余验收继续按下表单独记录。

| 问题 | 已提交代码 / 合同切片 | 已完成自动验证切片 | 未满足验收 |
| --- | --- | --- | --- |
| 001 · J | [x] 滚轮按协议边界拆分，保留总距离 | [x] 跨层滚轮边界回归 | [ ] 真实设备连续滚动/特殊应用 |
| 002 · J | [x] 持久远控surface与shell局部更新 | [x] DOM会话存活/清理回归 | [ ] 实际WebView持续输入与静态恢复 |
| 003 · A | [x] 可恢复error与终态分离 | [x] Rust错误事件/Windows静态合同 | [ ] Swift/Xcode及Mac真实恢复 |
| 004 · A | [x] 发送取消/截止与后台句柄退役 | [x] Rust堵塞发送/截止、Python teardown合同 | [ ] Mac真实背压/主线程退出预算 |
| 005 · A | [x] 物理字符/修饰键平衡与重复键边界 | [x] Python静态输入合同 | [ ] Swift回归与真实布局/按键释放 |
| 006 · A | [x] 生产NSTextInputClient组合/提交链 | [x] Python输入合同，Swift用例已提交 | [ ] Swift执行、拼音候选/取消/最终提交 |
| 007 · A | [x] 生产快捷键策略与原始组合入口 | [x] Python映射合同，Swift用例已提交 | [ ] Mac真实菜单/系统组合与切换释放 |
| 008 · J | [x] Host暂停/恢复、结束控制与peer终态确认 | [x] 本地状态/结束确认/取消回归 | [ ] 双机本机立即收回与权限场景 |
| 009 · J | [x] InputState、真实apply反馈与静默握手有界退出 | [x] 输入受限/恢复、旧端握手与联合回归 | [ ] 管理员/UAC/锁屏等真实权限矩阵 |
| 010 · J/M | [x] 静态恢复IDR并保留buffered输出请求 | [x] 媒体恢复与联合软件MFT链 | [ ] 真桌面/双机恢复时延 |
| 011 · J/M | [x] crypto短锁、独立probe窗口与取消树 | [x] 慢探测/重放/跨lane回归 | [ ] 双机黑洞输入/画面p95/p99 |
| 012 · J/M | [x] 输入独立消费、慢配置合并与完成回执 | [x] 慢控制/输入并发回归 | [ ] 慢驱动双机输入预算 |
| 013 · J/A/CB/V | [x] Windows完整u64/绘制/host映射、完整Apple ABI/owned future/media_generation与receipt/admission后继均已合入 | [x] V Rust本地FFI91、Windows292/17忽略及真实软件MFT/QUIC/Noise联合回环；J634/2/19仅旧源历史 | [ ] 6项Swift Recording、Swift/Xcode、实际presented与同stream完整拓扑映射；主项不因Rust本地通过关闭 |
| 014 · M | [x] CPU离线分段基线与真实输入/输出身份 | [x] 可复跑离线工具及buffered时序校验 | [ ] GPU/双机/端到端/功耗温度/长稳对照 |
| 015 · M/G | [x] G97ff已合入owned surface池/VP缩放转色/capture入口与生产runtime选路；V仅含M的历史范围不变 | [x] G25软件检查及共享GPU选中24用例通过，含池/lease/fence/预算/退役；代码/软件小项完成 | [ ] 真DXGI捕获/恢复、厂商/混合GPU、CPU/GPU同画质性能/帧龄/功耗/长稳；主项保留未完成 |
| 016 · M/G | [x] G97ff已合入tracked-sample GPU编码、原输出身份、同帧软件回退与版本边界 | [x] G25软件检查及共享GPU选中24用例通过，含真实合成surface/MFT编码、解码内容与epoch归属；代码/软件小项完成 | [ ] 真实driver/device-loss、真桌面/双机、厂商/混合GPU与同画质性能/恢复矩阵；主项保留未完成 |
| 017 · J | [x] 有界AudioWorklet音频播放 | [x] 环形缓冲/生命周期与前端回归 | [ ] 实际播放、欠载/延迟/分配率测量 |
| 018 · A | [x] 专用解码队列、字节/帧龄预算与合并presenter | [x] Python静态media合同，Swift调度器用例已提交 | [ ] Swift/Xcode、VideoToolbox与阻塞主线程性能；保留现有复制 |
| 019 · N | [x] 三类媒体独立有界sender及代数隔离 | [x] QUIC ACK黑洞、健康会话隔离及本机调度测量 | [ ] 真实WAN/双机/容量与音频体验 |
| 020 · M/J/V | [x] 可靠视频builder单次增量构造并复用buffer | [x] builder等价/边界、来源套件与V软件接入通过 | —；M主项完成，本切片候选软件缺项已收口 |
| 021 · J | [x] 两端有界磁盘worker/独占收尾及初始化取消preserve | [x] 原prefix保留/续传、慢盘隔离及J完整Windows套件 | [ ] 真实慢盘/云盘/杀毒/空间耗尽的媒体预算 |
| 022 · M/J | [x] GDI兜底中退避探测DXGI并迟滞恢复 | [x] 恢复状态/后端契约回归 | [ ] 真实UI诊断、睡眠/RDP/驱动恢复与帧龄 |
| 023 · M/J | [x] 事务ICodecAPI码率更新与预设内自适应 | [x] 策略/动态更新契约回归 | [ ] 实际支持硬件的效果与失败恢复；完整协商 |
| 024 · N | [x] opt-in认证八通道LAN/可靠回退/去重/随机收讫 | [x] localhost多lane、伪ACK、慢文件、013上下文保留 | [ ] 平台生产接线、实际LAN/WAN/恢复；未整体交付 |
| 025 · A/V | [x] 完整Apple历史、双C头生成、ABI布局/有界命令及Swift能力映射已合入 | [x] V真实MSVC C布局、双头检查、Rust/Python合同通过 | [ ] Swift/Xcode及Apple平台ABI运行 |
| 026 · A | [x] Mac屏幕/画质控制与远端回执状态切片 | [x] Rust codec fixture、Python会话合同 | [ ] Swift执行、实际音频/剪贴板/文件等其余功能闭环 |
| 027 · A | [x] 有界位置解码与Mac/iOS独立位置overlay | [x] Rust光标fixture/Python合同 | [ ] 完整shape/hotspot及真实几何/呈现效果 |
| 028 · A | [x] 授权/expiry/身份pin与Keychain重连模型 | [x] Rust授权/持久等待取消与Python保存设备合同 | [ ] 真Keychain迁移、长期重连、跨设备误连保护 |
| 029 · A | [x] Mac/iOS连接/审批/恢复取消与generation隔离 | [x] Python取消UI合同 | [ ] Swift和真实等待阶段取消/重连 |
| 030 · M→文件会话 | [x] 单次源hash、Windows只读snapshot与peer退出取消 | [x] 现有256MiB范围传输/取消回归 | [ ] 大文件/4GiB、目录、持久块证明；新增文件协议14由第七会话独占 |
| 031 · J | [x] 完整等待队列、稳定焦点与逐项移除 | [x] 队列/键盘焦点前端回归 | [ ] 实际多文件长队列操作体验 |
| 032 · J | [x] 原生drop绑定可见会话区域与目标身份 | [x] 命中/会话代数回归 | [ ] 多显示器DPI/双机原生拖放 |
| 033 · J | [x] 发送Esc/系统组合与宿主快捷键入口 | [x] 按下/释放平衡与路由回归 | [ ] 全屏/IME/布局与实际远端应用 |
| 034 · J | [x] 内存账号草稿与重发邮箱绑定 | [x] 表单失败/重试/地址回归 | [ ] 实际账号页网络失败体验 |
| 035 · J | [x] 完整三笔IPC所有权链与失败取消可重试 | [x] 迟到响应/持久化/host重启归属回归 | [ ] 实际登录/退出/换号与服务组合验证 |
| 036 · S/V | [x] 数据库原子注册与并发幂等 | [x] 本地并发/唯一键竞争与V最终服务回归 | [ ] 部署及真实运行环境验收 |
| 037 · S/V | [x] 可写数据库readiness与有界锁等待 | [x] 依赖不可用/锁竞争与V最终服务回归 | [ ] 实际运行环境与部署 |
| 038 · S | [x] leased邮件worker及账号历史生命周期 | [x] 本地投递状态/回收/恢复回归 | [ ] 真实邮件服务、长时故障恢复与部署 |
| 039 · S | [x] 索引/分片分批保留期清理 | [x] 本地清理/容量/并发接收回归 | [ ] 长期磁盘/生产性能与部署验证 |
| 040 · E/V | [x] transport单一穷尽恢复策略，Host/Controller两生产入口删除重复match并已合入V | [x] 原策略29/18/12、三包严格Clippy/fmt与V完整软件门禁 | [ ] 其余实际职责拆分、HostError字符串类型化及性能比较；不扩大本策略角色，040主项保留未完成 |
| 041 · E/V | [x] 分层入口、固定工具/源码报告、MSVC真实C环境与完整relay门禁 | [x] 干净a4的25检查、FFI91与本地签名→HTTP/SQLite19案例通过 | [ ] native仍2通过/7跳过、incomplete；Swift/平台/设备/性能待验，软件收口子项完成而主项不关闭 |
| 042 · E/V | [x] 当前能力/版本/完整来源与逐项进度同步 | [x] V固定源与完整报告、历史失败和来源状态分别记录 | [ ] main合入后的状态同步；本次不宣称已合main/推送/部署 |
| 043 · A | [x] Main/1080p与实际profile/尺寸安全拒绝 | [x] Python2项合同，Swift用例已提交 | [ ] 动态完整协商、Swift/真实Mac编码与权限 |

新增044–061不复制root未知作者的未提交草稿；当前已核实来源切片另列如下，未获证据者继续待核实：

| 问题 | 已提交/核对的切片 | 未满足验收或成立性 |
| --- | --- | --- |
| 044 · J/M/S/D/V | [x] cursor降级/typed observer、全u64 producer/service/GUI合同及V本地签名/store/analysis全值域19案例 | [ ] 设备GUI/双机、远端生产送达；observer不计真实丢包 |
| 045 · J/M/D/S/V | [x] Some(Err)反证、None退役/迟到probe隔离、全u64合同与V本地签名/store/analysis19案例 | [ ] 设备GUI/真实监听、远端生产送达；不延续“任一Err永久禁用” |
| 046 · M/J/V | [x] 路径所有权/按连接身份退役/迟到结果取消及V软件接入通过，M主项完成 | —；来源及本候选软件切片已收口 |
| 048 · E | [x] Noise message3认证payload链路已核对 | [ ] P1/UKS攻击路径证据不足，熵源/篡改/拼接待独立证明；未直接改crypto |
| 049 · N/V | [x] 按relay IPv4/IPv6族绑定本地socket，真实::1/v4及V完整relay回归 | [ ] 真实WAN IPv6/NAT/双栈路由另验 |
| 050 · M/J/V | [x] 重复chunk保留首payload且不延寿、来源及V软件回归，M主项完成 | —；来源及本候选软件切片已收口 |
| 051 · A | [x] 网络/服务故障保留账号会话、generation隔离及旋转token取消落盘；Python10项合同 | [ ] Swift行为用例17项未运行，真实Keychain/断网/重新登录 |
| 052 · N/V | [x] QUIC资源预算、join前datagram防护、V完整relay回归；容器/cgroup为N来源实测 | [ ] 容量结论只限已测载荷，生产环境另验 |
| 053 · N/V | [x] 绝对join/partial-frame截止与V完整relay回归；512连接probe保留N来源身份 | [ ] 部署长稳 |
| 054 · S/V | [x] 诊断验签前准入与事务持久配额、最终服务已聚合并通过V本地回归 | [ ] 部署容量 |
| 055 · S/V | [x] 可信socket/显式代理来源、有界限流键与token动作独立分桶补修，V最终服务回归通过 | [ ] 部署环境与长期负载 |
| 056 · S/V | [x] 哈希/响应窗/计算门控、resetPassword预算先于token消费补修与V最终服务回归 | [ ] 部署空闲/饱和枚举 |
| 057 · S/N/V | [x] 最终服务来源与V跨进程关闭回归；N Linux候选关停为来源证据 | [ ] 实际部署关闭/重启与长稳 |
| 058 · S/V | [x] 设备标签账号隔离、越权/换号与V最终服务回归 | [ ] 部署验收 |
| 059 · A | [x] 未知Host事件固定计数/不解析、Transfer非终态；Windows2静态合同 | [ ] Swift及真实Transfer全流程 |
| 060 · A/V | [x] 认证会话时钟校准/context先验隔离、完整Apple已聚合并通过V Rust本地回归 | [ ] 真Mac时钟跳变/输入注入 |
| 061 · A/V | [x] direct绝对静默截止、真实loopback黑洞/持续收包红绿回归与V软件接入 | [ ] Mac/iOS实际启用、双机/防火墙 |
| 047 | [ ] 成立性、来源/owner与实跑范围待核实 | [ ] 保留root未提交稿，不能总括全部确认或修改其owner |

## 2. 本次验证基线（初始记录，后续源码结果见上表）

以下均为本次在当前 Windows checkout 上执行的结果，不是引用历史记录：

| 验证 | 结果 | 证明范围 |
| --- | --- | --- |
| Windows 前端，Bun 1.3.14 `bun test` | 246 通过，0 失败 | 现有逻辑、契约和合成宿主测试 |
| Windows 前端 `bun run build` | TypeScript 与 Vite 构建通过 | 前端类型及生产构建 |
| account，Bun 1.3.14 | 20 项测试与 typecheck 通过 | 本地账号模块 |
| diagnostics，Bun 1.3.14 | 14 项测试与 typecheck 通过 | 本地诊断接收与分析模块 |
| Python `unittest discover` | 102 项通过 | 现有脚本用例；其中探针重试为测试夹具，不是线上可用性结论 |
| protocol / video / session / transport / crypto | 所选 `--lib --tests --locked` 全部通过 | 五个共享 crate 的本地单元及集成用例 |
| `desklink-ffi --lib` | 12 项通过 | FFI 本地单元测试 |
| `desklink-windows-ui --lib` | 90 项通过，1 项忽略 | 桌面管理层单元测试；忽略项要求显式提供已构建的签名更新包 |

额外的本地复现：

1. 阻塞 `RemoteInputDispatcher` 的首个发送，累计 13 次 `deltaY=100`，实际发送合并滚轮值 **1300**；Rust 协议上限是 **1200**。见 DL2-001。
2. 同一个内存账号库并发提交两次相同邮箱注册，返回 **202 与 500**，后者为 `users.email` 唯一键冲突。见 DL2-036。
3. 关闭账号数据库后调用同一 app 的 `/ready`，仍返回 **200 / ready**。见 DL2-037。
4. 在真实 `DiagnosticStore` 的内存库执行保留期删除的 `EXPLAIN QUERY PLAN`，得到 **SCAN events**。见 DL2-039。

2026-10-26 复核补充：重跑 Windows 前端 `bun test` 为 246 通过、0 失败；草稿引用的 30 处 file:line 证据逐条读源码重验，全部成立（DL2-008 的 stop 位置为 `apps/windows-ui/src-tauri/src/host.rs:502`，DL2-010 的 Idle 分支实际含首帧 10 秒 deadline 检查，DL2-017 的常量在 14-15 行）。新增 DL2-044～061 的证据均经源码逐条抽查核实；服务端与共享 crate 条目由独立专项审查发现，关键项（relay 流控、握手密钥、时序枚举、HostBridge 默认分支、AccountClient 恢复路径、smartPasteBusy、光标致命错误、direct accept 折叠）另由主会话二次确认。

本次没有运行 Swift/Xcode、双机远控、GPU/CPU 性能采样、长稳、真实 UAC/锁屏或公网故障注入。CodeGraph 用于定位符号、依赖与影响范围，结论均再以源码确认。三个专项审查覆盖 Windows 产品流程、原生媒体/传输、Apple/FFI；重连错误与加密锁问题另作交叉复核。

## 3. 核心正确性、输入与恢复

### DL2-001 · P1 · 修复滚轮合并与 Rust 上限冲突

- [x] 统一跨层输入边界，发送时拆分合并滚轮事件。
- **修复 / 自动验证：** `fix(DL2-001): preserve wheel distance within protocol event bounds`；前端常量由 Rust 协议生成，build 检查漂移；累计不截断，按 1200 拆分并保留反向、按钮顺序、stream 及取消边界。Bun 1.3.14 `bun test src/remote-input-dispatcher.test.ts src/remote-input.test.ts` 27 通过；`bun run build` 通过。5000/10000 单位合成背压、水平与负向、取消已覆盖；未做真实输入注入。
- **证据 / 类型：复现缺陷。** `apps/windows-ui/src/remote-input-dispatcher.ts:18,49-57` 将滚轮合并至 4800；`crates/desklink-protocol/src/lib.rs:70` 上限 1200；`apps/windows-ui/src-tauri/src/controller.rs:4656-4663` 拒绝超限。现有 dispatcher 测试反而把 4800 当作正确输出。
- **用户影响：** 快速滚动且 IPC 有背压时，整个合并事件被拒绝，滚动距离丢失；`apps/windows-ui/src/controller.ts:275-277` 的空错误处理令失败不可见。
- **方案：** 由共享契约产生边界常量；累计量和单事件发送上限分离；按合法分片发送并保留方向、总量及按钮/按键顺序。不能用简单 clamp 丢弃剩余滚动量。
- **验收：** 堵塞发送器后累计 5000 单位，每个实际进入 Rust 的事件均不超 1200，合计仍为 5000；覆盖正负方向切换、水平滚动、取消和 stream 切换。

### DL2-002 · P1 · 让远控表面独立于全局页面重绘

- [ ] 将视频、解码器和输入承载改为持久会话表面，普通界面更新只修改局部。
- [x] `fix(DL2-002): retain the active remote surface across shell updates`：以 stream/config/解码失败代数标识会话表面，同代数仅更新侧栏、顶部菜单、通知和审批，保留画布/输入承载/解码器，不重新绑定远端输入。真正离开会话、账号门禁或配置变化仍先释放资源；菜单/审批真实夺焦仍走原 blur ReleaseAll。Bun 1.3.14 渲染生命周期/IME/会话表面 21 项通过，生产构建通过。
- [ ] 双机持续按键、静态画面下 Host/菜单/审批实际刷新与 DL2-010 联合恢复；当前合成宿主验证未测停顿和真实关键帧次数。
- **证据 / 类型：源码缺陷、性能结构。** `apps/windows-ui/src/main.ts:156-170` 每次 render 都调用 `prepareControllerRender` 并替换整个 app；`apps/windows-ui/src/controller.ts:372-395` 无条件释放输入、关闭解码器及展示资源。打开更多菜单（`apps/windows-ui/src/main.ts:1610-1612`）、本机 Host 状态变化（`1998-1999`）也能触发此路径。
- **用户影响：** 无焦点变化的本机共享事件也会重置连续输入；普通菜单更新会额外引发解码器重建和关键帧请求。菜单真正取得焦点时释放远端按键是必要保护，不能当作缺陷。实际停顿时长未测。
- **方案：** 会话、视频配置、页面可见性分别建模；shell、通知、审批和菜单局部更新；只在真正离开会话、配置代数变化或终态销毁资源。现有账号页离开时的清理要求仍应成立。
- **验收：** 持续输入与静态画面期间，无焦点变化的 shell/Host 更新不重置输入；菜单/审批真正取得焦点时正常 ReleaseAll，但均不因普通刷新关闭解码器或额外请求关键帧；真正退出会话仍清理资源。与 DL2-010 联合验证静态恢复。

### DL2-003 · P1 · 修复 Apple 把可恢复错误当终态

- [ ] 将恢复告警和终态错误分开，保留正在自动重试的 FFI runtime。
- **证据 / 类型：源码缺陷，已交叉复核。** `crates/desklink-ffi/src/worker.rs:558-568` 发送 Reconnecting 后发送 state 仍为 Reconnecting 的 Error；`apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:339-341` 忽略 stateValue，`517-523` 直接 disconnect 并置 failed。
- **用户影响：** 当前代数中的可重试网络故障会由 Swift 主动取消底层恢复。旧代数回调已有过滤，不属于本问题。
- **方案：** 错误事件携带稳定 code、可恢复性和状态；恢复期更新状态及提示，认证拒绝、明确失效或用户断开才销毁句柄。错误文字不再承担状态判断。
- **验收：** 注入 Reconnecting → 可恢复 Error → 新 VideoConfig → Frame，句柄保持有效且恢复画面；终态认证错误才关闭；macOS → Windows 切 Wi-Fi、relay 重启再做真机回归。

- [x] 代码与 Rust 自动回归：Error 携带稳定 error_code/recoverable；Swift 恢复告警仅更新提示，Connected 清告警，终态仍销毁。`python scripts/run-windows-cargo.py test -p desklink-ffi --lib --locked`：13 通过。修复提交 `30a902a`（主题 `fix(DL2-003): preserve Apple runtime during recoverable errors`）。
- [ ] macOS Wi-Fi/relay 重启与 VideoConfig→Frame 恢复验收；已补 Swift 生命周期回归，本 Windows 环境未执行。

### DL2-004 · P1 · 已连接发送背压时仍能及时取消和退出

- [ ] 为发送、关闭及 FFI teardown 建立可取消、有截止时间的异步生命周期。
- **证据 / 类型：源码缺陷。** `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:33,299-305` 在 MainActor 同步 reject；`crates/desklink-ffi/src/lib.rs:620-622,1099-1100` 最终调用 `crates/desklink-ffi/src/worker.rs:223-227` 的 thread.join。`crates/desklink-ffi/src/worker.rs:449-502` 外层 select 有取消，但分支内 send_input / request_keyframe 的 await 没有同等取消保护；`crates/desklink-transport/src/quic.rs:963-1002` 写流仍可能等待。复核补充：`crates/desklink-ffi/src/lib.rs:1656-1671` 在回调线程内调用 host destroy 时，detached 线程只 join 事件线程、从不清除回调（`handle.callback.clear()` 仅在非回调线程路径执行），destroy 返回后残余事件仍可能以悬空 context 回调 C 侧，对调用方构成 use-after-free 式回调。
- **用户影响：** 对端停止读取或可靠流背压时，断开、退出或场景切换可能阻塞 Apple 主线程。
- **方案：** 发送本身参与 cancellation/deadline；关闭 transport 唤醒等待者；将阻塞 join 移出 UI 线程，以停止完成事件管理 callback context 的释放，避免为了快速返回制造回调悬空。
- **验收：** 本地 peer 建连后停止读流、队列满时取消，UI 仍响应；建议关闭完成预算 ≤2 秒，超时有明确清理结果；随后能重新连接。连接阶段超时用例不能替代此项。

- [x] 发送 await 本身增加 cancellation + 1 秒截止（含 ReleaseAll/关键帧）；Swift 先退役句柄，再后台 destroy/join，回调上下文仅在 join 后释放。Rust 15 单元测试通过（新增已开始且堵塞发送的取消、无取消截止、新会话独立 token）；Python teardown 3、取消 UI 5 项合同通过。提交主题 `fix(DL2-004): cancel blocked sends and retire Apple handles off main thread`。
- [ ] 实际本地 QUIC peer 停止读取、队列满的 ≤2 秒关闭/重连及真 Mac UI 响应实测；合成有界 channel 不等于真实流控证明。

### DL2-005 · P1 · 补齐 macOS 字符按键的释放语义

- [ ] 分离物理按键与提交文本，保证按下 / 松开及修饰键状态平衡。
- [x] 代码与静态合同：物理字符保留原始 scalar / modifiers，repeat 复用按下记录，keyUp 成对发送；flagsChanged 差量同步修饰键，失焦、会话状态变化和销毁清理；提交文本按 scalar 发送 down/up，Mac Host 保留 Unicode pressed 并追踪失败释放。修复主题：`fix(DL2-005): balance Apple physical character and modifier releases`。
- [x] Windows 自动验证：`python -m unittest scripts.tests.test_dl2_mac_input_contract -v`，6 项静态源码合同通过；`git diff --check` 通过。
- [x] 后续边界切片：长按期间字符 / chord 变化先释放旧逻辑身份再按下新身份，最终松开匹配新记录；Caps Lock 只响应真实锁定键的 flagsChanged，不因重新取得焦点后的其他修饰键而猜测 / 切换远端状态。当前静态合同 12 项通过，新增相应 Swift 回归尚未在 Mac 运行；主题 `fix(DL2-005): balance repeats across glyph changes and physical caps events`。
- [ ] macOS 自动 / 真机验收：新增 Swift 状态、repeat、FFI struct 编码、Host Unicode 释放与失败重试回归，但 Windows 无 Swift/AppKit，尚未运行；大写、快捷键、拖动、长按、失焦与快速重连的双机无粘键结果仍待真 Mac。
- **证据 / 类型：源码缺陷。** `apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift:15-16` 对普通字符只在 keyDown 发 unicode；`apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:248-252` 只发送 pressed=1；Windows `apps/windows/src/input.rs:359-379` 依赖 pressed=false 生成 keyUp 和修饰键释放。
- **用户影响：** 字符及 Shift/Ctrl/Alt/Meta 组合存在残留按下状态路径，可能要等失焦或断开才整体释放。不能用“文字能显示”证明按键状态完整。
- **方案：** 物理键记录 press/release/repeat；文本提交使用原子文本语义或完整 down/up；flagsChanged 差量同步修饰键；失焦、权限变化和断开统一 ReleaseAll。
- **验收：** 端到端断言所有字符/修饰键事件平衡，覆盖大写、快捷键后输入、拖动、长按、失焦及快速重连，无粘键。

### DL2-006 · P1 · 为 macOS 接入真实本地输入法

- [ ] 接通 AppKit 组合文本、候选、最终提交和文本发送链。
- [x] 代码接线：生产 InputView 实现 `NSTextInputClient`，`keyDown → interpretKeyEvents → setMarkedText / insertText / doCommand` 接入系统输入上下文；组合文本及 UTF-16 选区留本地，最终提交按文本发送；候选取消、失焦和会话切换丢弃组合，避免透传 Escape / 空格。修复主题：`fix(DL2-006): connect AppKit composition to remote text commits`。
- [x] Windows 静态验证：`python -m unittest scripts.tests.test_dl2_mac_input_contract -v`，9 项源码合同通过；`git diff --check` 通过。新增 5 项 Swift 输入上下文回调与最终 FFI 编码回归，尚未在 Mac 运行；这些回调合同不代表真实拼音验收。
- [ ] Mac 自动与真机：拼音、中文标点、选词、中英切换、Emoji、死键、多行和候选取消仍需 Swift/Xcode 与真 Mac；候选坐标目前随最后一次远端点击及视频布局变化，协议尚未提供远端 caret 几何，远端用方向键移动插入点后的精确候选跟随仍未完成。
- **证据 / 类型：能力缺口。** `apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift:27,66-67,97-105` 是直接读取 event.characters 的 NSView，没有 NSTextInputClient、interpretKeyEvents、insertText 或 marked-text 管理。现有人工传入“中”的 mapper 测试不覆盖真实输入法。
- **方案：** 使用原生文本承载或实现 NSTextInputClient；组合期间不转发拼音和选词键，提交后有序发送最终文本；候选窗口定位跟随远程输入焦点；与 DL2-005 共用清晰的文本/按键契约。[Apple 文本输入接口](https://developer.apple.com/documentation/appkit/nstextinputclient?language=objc)规定了自定义视图需要接入的组合文本方法。
- **验收：** 真 Mac 拼音、中文标点、中英切换、退格、回车/空格选词、Emoji、死键及多行输入；取消候选不向远端多发 Escape/空格。

### DL2-007 · P1 · 将 Mac 快捷键映射接入实际输入路径

- [ ] 统一“按本机习惯 / 发送原始按键”的策略与生产调用入口。
- [x] 代码接线：同一 `KeyboardMappingPolicy` 驱动生产 keyDown、flagsChanged 与 AppKit `performKeyEquivalent`；仅已认证 Windows 的自动模式把 Command 映为 Ctrl，未确认 / Mac / iOS 及原始模式保留 Meta。切换映射或平台时释放旧输入；会话“键盘”菜单可选模式并发送 Alt+Tab、Windows 键和 Win+R 等原始系统组合。修复主题：`fix(DL2-007): route Mac shortcuts through negotiated keyboard policy`。
- [x] Windows 自动验证：`python -m unittest scripts.tests.test_dl2_mac_input_contract -v`，11 项静态源码合同通过；Apple 取消 / cursor / media / teardown 另外 20 项合同通过；`git diff --check` 通过。新增 Swift 生产 InputView → Bridge → FFI struct 回归，覆盖 C/V/A/Z、原始及未知平台、key equivalent 和切换时释放。
- [ ] Mac 自动与双机：Windows 无 Swift/AppKit，新增 Swift 测试尚未运行；真实 Mac → Windows 的复制、粘贴、全选、撤销、窗口切换与原始 Win 键仍待验收，不以 helper / 静态合同替代。
- **证据 / 类型：源码缺陷。** `apps/macos/Sources/DeskLinkApp/Input/InputMapper.swift:45-53` 有 automatic 的 Command→Control，但 `apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift:97-105` 使用 KeyboardMapper；`apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift:95` 实际把 Command 映为 Meta。remoteModifier 的相关测试只覆盖孤立 helper。
- **用户影响：** Mac 用户常用 Command+C/V/A/Z 控制 Windows 时，实际可能变成 Win 组合键。
- **方案：** 根据已协商的远端平台映射，默认符合用户习惯，原始 Win/Command 模式可选择；会话菜单显示必要系统组合键。不要只修测试 helper。
- **验收：** 断言真实发入 FFI 的事件；Mac → Windows 复制、粘贴、全选、撤销、切换窗口与原始 Win 键均正确。依赖 DL2-005、025。

### DL2-008 · P1 · 被控端能够独立收回控制、暂停和恢复共享

- [ ] 提供“结束当前控制”“暂停共享”“恢复共享”三个独立动作。
- [x] 暂停/恢复切片 `feat(DL2-008): persist independent Host sharing pause controls`：共享页、顶部状态区及托盘均可操作；只停止 Host，信任和固定密码保留、临时配对失效；独立 marker 跨重启保存，账号/电源恢复及配对回落尊重暂停，保存失败仍关闭本次共享。管理库 108 项通过、1 签名包忽略；新增 6 项原生暂停/存储/诊断测试；Bun 1.3.14 状态/持久表面/实际操作宿主 11 项通过，构建通过。
- [ ] 双角色/重启/睡眠与真实网络中断下的结束控制验收；合成终态及 Host 再次连接已完成。
- [x] 结束控制切片 `feat(DL2-008): end authenticated Host control with terminal acknowledgement`：本机 stream 绑定命令先停止输入/媒体，再通过认证控制通道发送现有 HostUnavailable 终态并等待对端断开；确认后 Host 可接受新请求，未确认则报错并持久暂停共享。共享页、顶部与托盘提供独立“结束当前控制”。真实本地 relay + Noise + 生产 Controller 合成测试证明两次终态拒绝及同 Host 重新连接；不读消息 peer 返回失败。Windows 库 106 通过、1 桌面忽略；管理库 116 通过、1 签名资产忽略；前端相关测试与构建通过。没有启动真实捕获或注入输入。
- [x] 替换 peer 补修 `fix(DL2-008): require the ended peer to disconnect before confirming`：PeerReplaced 不再作为结束确认，独立等待原 peer 断开，替换/超时走持久暂停保护；普通授权拒绝流程不变。真实本地替换 peer 回归通过，session_control 聚焦 5 通过。后续整合全库数据见对应子项。
- **证据 / 类型：能力缺口。** `apps/windows-ui/src/main.ts:786-793` 共享操作集中在密码；`apps/windows-ui/src-tauri/src/lib.rs:1677-1741` 未暴露停止 Host / 结束当前 Host 会话命令，`1752-1770` 托盘只有打开/退出。`apps/windows-ui/src-tauri/src/host.rs:502` 已有底层 stop 能力。
- **用户影响：** 要保留信任却立即结束本机被控状态，缺少直接入口；退出整个应用还会结束本机作为 Controller 的另一条连接。
- **方案：** Host 与 Controller 生命周期分离；共享页、托盘及被控状态条均能立即收回控制；明确保留信任、临时密码处理及重启后是否继续暂停。暂停不得被账号恢复或电源恢复静默覆盖。
- **验收：** 双角色并行时停止 Host 不影响 Controller；新连接被拒绝，输入与媒体及时停止；恢复共享后保留的信任仍可用；连续点击、重启和睡眠唤醒状态一致。

### DL2-009 · P1 · 让“有画面但无法操作”可解释、可恢复

- [ ] 将持续输入受限转为独立会话状态，并补齐权限场景指引。
- [x] 共享契约切片 `feat(DL2-009): add protocol 13 input restriction state`：唯一业务协议升至 13，末尾追加 InputState，三个稳定原因及 None 恢复；保留 relay JOIN 信封版本。协议/FFI 77 项、目录 6 项、Windows 原生升级提示 1 项测试通过，覆盖非法原因/截断、旧 tag、目录 12↔13 与密码先验、收到旧版 Noise/媒体拒绝。当前仅契约，未宣称输入反馈已接通。
- [x] 输入反馈切片 `feat(DL2-009): report sustained input restrictions without disconnecting`：当前认证会话内按输入类别累计三次同因失败，去重发送有界最新状态；释放/零输入和其他类别成功不误清警，真实同类恢复后自动清除。发送等待独立于输入处理；控制端仅更新提示，不改连接状态，断开/新 stream 清旧提示。Windows 库 102 通过、1 交互桌面测试忽略；管理库 113 通过、1 签名资产忽略（含稳定 signal 回归）；前端 312 项全过，构建通过。权限边界见 `docs/windows-input-restriction-protocol13.md`，均未冒称真实 UAC/锁屏可控。
- [x] 复用媒体组 `4dabcde` 的输入快路径切片，将输入限制 tracker 迁入 receive_host_input，保留独立有界反馈发送 future，慢配置不阻塞输入。整合后 Windows 库 107 通过、1 桌面忽略，管理库 123 通过、1 签名资产忽略；包含慢配置、输入反馈取消及结束控制回归。
- [ ] 真实新旧直接/保存连接（包括已安装旧二进制）的静默握手退出/提示、混合版本设备，以及管理员窗口/UAC/安全桌面/锁屏验收；共享协议、Windows 持续失败汇总/恢复、前端提示与合成握手预算的已完成部分见各子项，不能据此推定真实权限或所有平台场景已验收。
- [x] 静默握手预算切片 `fix(DL2-009): bound unanswered native handshakes independently of offline retries`：仅已建 peer 的 HandshakeTimeout 累计三次后终止；离线传输失败不消耗预算，认证成功重置。稳定原因是 handshake_no_response，准确提示检查在线、网络和两端版本，不推断已证实不兼容；FFI 保留现有六次重试预算。生产分类/离线/认证/持久连接测试通过；最终管理库 123 通过、1 签名资产忽略，Windows 库 107 通过、1 桌面忽略。
- [x] 主机版本拒绝切片 `fix(DL2-009): reject old initiator handshakes without silent retries`：只对明确 UnsupportedVersion 在原 peer generation 上限时 500ms 回复当前 Noise 版本并结束尝试，畸形/过期包不误当升级问题。真实本地 relay 回归 2 通过，覆盖回复在重设通道后可读及同 Host 再次接受当前版本 Noise；旧端使用明确标注的协议 12 守卫夹具，未运行旧二进制或真实远控。
- **证据 / 类型：源码缺陷、能力边界。** `apps/windows/src/input.rs:306-326` 返回 Blocked；`apps/windows/src/runtime.rs:2714-2723` 把 Blocked / UnsupportedKey / InvalidInput 都转为 Ok。`apps/windows/assets/desklink.manifest:8` 为 asInvoker、uiAccess=false。
- **用户影响：** 输入持续失败时，控制端仍只看到已连接；无法区分权限限制、不支持的键或远端没有响应。
- **方案：** 临时失败保留会话，持续失败按稳定原因汇总反馈并自动解除；对普通桌面、管理员窗口、UAC、安全桌面、锁屏分别定义支持范围；需要提权或受信服务时采用明确的本机授权流程。
- **验收：** 每种权限场景有实机结果；输入受限及时提示可执行的下一步，恢复后自动消除；不得把暂时失败升级为断线风暴，也不能继续静默成功。UAC 可控性本次未实测。

### DL2-010 · P1 · 静态桌面也能响应后续关键帧请求

- [ ] 为恢复请求增加复用最近有效帧或强制快照的产帧路径。
- **证据 / 类型：源码缺陷，已交叉复核。** `apps/windows/src/runtime.rs:1965-1990` 遇到 CaptureOutcome::Idle 在首帧 10 秒 deadline 检查后 continue（已产出过帧的静态桌面直接跳过），`2033-2037` 才处理 force_keyframe；`2537-2542` 请求只设标志。`apps/windows/src/capture.rs:491-499` 的 GDI 兜底只保证首次快照。
- **触发条件：** DXGI 已产生首帧后桌面完全静止，控制端因解码器重建或丢失关键帧要求恢复。周期 GDI 不受同一问题影响；relay 已有可靠关键帧发送，但不能替主机生成新帧。
- **方案：** 请求独立唤醒媒体生产者；以配置版本绑定最近合法帧，限频重编码或强制捕获；不把静态桌面改为常态高频全屏轮询。
- **验收：** 静态桌面主动重置解码器/丢弃 DirectLan 关键帧后，无需移动鼠标或窗口即可恢复；建议请求至显示 ≤1 秒；覆盖与 DL2-002 的组合场景。

- [x] **已实现并自动验证（`fix(DL2-010): recover requested keyframes on idle desktops`）：** DXGI Idle 时响应待处理关键帧，按需抓取当前屏幕的独立 GDI 快照；250ms 限频，捕获循环最多一次50ms等待，不增加正常静态桌面的持续截图。配置切换后从当前输出抓取，不复用旧纹理。
- **验证：** `python scripts/run-windows-cargo.py test -p desklink-windows --lib static_keyframe --locked`，1通过；覆盖无请求零快照、连续请求限频及间隔后可重试；Windows原生库编译通过。
- [ ] **仍待真机：** 静态桌面重置解码器、DirectLan丢关键帧及DL2-002组合的请求至显示≤1秒验收；本次未建立真实远控会话。

- [x] **缓冲编码器跟进（`fix(DL2-010): retain idle recovery through buffered encoder output`）：** NeedMoreInput时保留关键帧/首输出恢复意图，使静态DXGI在250ms预算后再次提交快照；`--lib keyframe_recovery --locked` 2通过，覆盖缓冲结果后仍能触发下一次限频快照。

### DL2-011 · P1 · DirectLan 探测不能长期占用会话加密锁

- [ ] 将探测网络等待移出共享加密锁的临界区。
- **证据 / 类型：源码缺陷、性能结构，已交叉复核。** `apps/windows/src/runtime.rs:2652-2661` 出站探测持 secure guard 等待 endpoint.connect；`1575-1588` 的各通道 seal/open 共用此锁。`crates/desklink-transport/src/direct_probe.rs:393-431` 内有连接、开流及响应等 await。
- **边界：** 已有并发槽位和逐阶段 3 秒超时，问题不是无限死锁。入站 `apps/windows/src/runtime.rs:2741-2751` 也持锁，但外层 select 可在输入/控制就绪时取消该 future；不能写成入站必然阻塞输入 3 秒。复核补充：FFI 控制端存在同类模式——`crates/desklink-ffi/src/controller.rs:803-814` 入站探测持 secure 锁跨 `accept_probe_connection` 的多个 3 秒阶段（最坏约 9 秒），期间 `send_input` 的 seal（`controller.rs:311-318`）与控制消息加密全部排队。
- **方案：** 短锁生成/验证探测消息，网络 I/O 在锁外执行；必要时派生专用探测上下文，仍绑定已认证会话；停止时取消任务并释放连接。
- **验收：** relay 正常而候选黑洞、慢响应、半包时，测锁持有时间、输入及画面 p95/p99；不能出现由探测锁造成的秒级停顿，探测失败正常回退。Windows 原生与 FFI 两条路径分别覆盖。

- [x] **已实现并自动验证（`fix(DL2-011): release shared crypto lock during direct probes`）：** transport增加shared探测接口，原有独占接口复用同一状态机；seal/open短锁，连接/开流/读写无锁；Windows出入站均切换，出站任务由JoinSet随peer代数退出取消。
- **验证：** `test -p desklink-transport --lib direct_probe::tests --locked` 8通过；新回环在已建TLS但未响应探测时100ms内仍取得crypto锁并成功加解密Control；入站无流时30ms取消后锁可取得。`test -p desklink-windows --lib direct_video_tests --locked` 2通过（经scripts/run-windows-cargo.py）。
- [ ] **仍待双机测量：** 黑洞、慢响应、半包期间的输入/画面p95/p99和锁持有分布。原实现有阶段超时；入站原select可取消，此修复不把它描述为无限死锁。

- [x] **并发加密跟进（`fix(DL2-011): isolate delayed probes from video replay windows`）：** 短锁后探测不得与视频共用128包重放窗；追加DirectProbe=8独立密钥/计数/窗口，现有lane 0～7不变，所有probe入口统一使用。未另升业务协议版本。
- **验证：** crypto+transport `--lib --tests --locked` 79通过；真实共享endpoint探测等待期间推进256视频包仍完成认证；双向迟到probe、重放与跨lane拒绝均覆盖。修复前共享视频lane的窗口会把这类迟到密文误拒，不能仅把seal移到connect后缩短概率。

- [x] **加密边界补充（`test(DL2-011): cover delayed responses and probe lane authentication`）：** probe测试12通过、transport lib+tests严格Clippy通过；新增干净窗口错lane认证失败后正确打开仍成功、反射包拒绝不污染双向真实包、真实QUIC响应已seal后等待期间共享锁100ms可取得且256个反向视频密文不挤出probe。视频密文在真实共享session上处理；不把这项fixture描述为已认证前QUIC视频数据面测试。DirectProbe自身仍是128包窗口，不保证无限乱序，新旧probe实现需同批构建或失败回落。

### DL2-012 · P1 · 编码器重配与显示切换不能阻塞输入消费

- [ ] 将慢控制编排与输入快路径分开。
- **证据 / 类型：源码缺陷、性能结构。** `apps/windows/src/runtime.rs:2528-2706` 用同一 select 消费 control/input；控制分支等待显示切换（`2552-2560`）、画质切换（`2583-2586`）和自动降档（`2613-2622`）；配置应答等待在 `2476-2502` 最长 5 秒。
- **用户影响：** 编码器繁忙或重建变慢时，切画质和自动调节会让键鼠等待同一循环返回。
- **方案：** 带代数的异步配置请求与完成事件；合并过时请求，单独消费输入；ReleaseAll、取消和断开优先，配置完成不回写旧 stream。
- **验收：** 注入 2～5 秒重配置延迟，键鼠和释放事件仍满足预算；无积压输入突然回放，无旧配置覆盖新选择。

- [x] **已实现并自动验证（`fix(DL2-012): consume input independently from slow host controls`）：** peer内输入与控制改为两个并行且共同取消的future；重配5秒等待不再阻止输入解密/消费。显示切换独立通知释放，过渡期间丢弃坐标/按下事件而允许按键/鼠标释放；断开取消两条路径，不派生迟到输入任务。
- **验证：** `test -p desklink-windows --lib slow_configuration --locked` 1通过，合成5秒控制等待期间输入/释放队列在250ms预算内完成；`clippy -p desklink-windows --lib --locked -- -D warnings`通过（均经scripts/run-windows-cargo.py）。
- [ ] **仍待实机：** 原生延迟重配下键鼠p95/p99、无积压回放及显示切换/配置完成的双机回归。

- [x] **控制编排切片（`fix(DL2-012): coalesce configuration work beside control consumption`）：** 控制select独立轮询单个配置future，Display/Quality/Profile各保留最新1项、最多3待执行+1进行中；serial过滤旧完成，跨类别保序。快速新请求、关键帧及断开不再等5秒配置应答。显示选择的独立gate跨拓扑发布保留，直到最新选择完成，防止旧完成提前恢复输入。
- **验证：** `--lib configuration --locked` 9通过，含实际生产future被慢worker挂起时1000次选择只下发首项+最终项、旧完成失效、取消后迟到oneshot拒绝，以及5个固定队列/代数回归；lib+tests严格Clippy通过。此前完整lib 138通过/2忽略。

### DL2-013 · P1 · 连接中刷新显示拓扑并稳定识别屏幕

- [ ] 将显示列表、活动屏幕、视频配置和输入坐标作为带版本的拓扑快照。
- [x] Apple 输入生产侧切片：按实际呈现 context 捕获事件；Mac key / modifiers / mouse-button / IME 与 iOS 键盘 / 手势 / 延迟长按保留原 stream / epoch，keyup、文本整段和合成释放不重新贴当前标签。仅 stream / inputEpoch / runtimeGeneration 的目标屏障取消旧组合与 gesture；纯 configVersion 画质重配保留按键 / IME / gesture 与原始 context，跨目标屏障 repeat 不重建 press；UI 清理同步或按原 context 释放，Mac 输入几何使用已呈现 rect。主题 `fix(DL2-013): retain captured presentation context across Apple input lifetimes`。
- [x] Windows 可运行验证：`python -m unittest scripts.tests.test_dl2_mac_input_contract scripts.tests.test_dl2_apple_input_capture_contract scripts.tests.test_dl2_mac_session_contract -v`，27 项源码合同通过；新增 Swift 事件模型 / FFI 编码回归覆盖迟到 keyup、旧 modifier、IME 屏障与长按 ticket，但本机无 Swift/AppKit/UIKit，尚未执行，不能代替 Apple 编译或硬件呈现验收。
- **证据 / 类型：源码缺陷。** `apps/windows/src/runtime.rs:1597-1622` 只在会话建立时枚举；`1414-1417` 保存不可变列表，`2545,2565-2571` 按旧列表校验并回发；`1992-2015` 恢复捕获时未刷新列表。`apps/windows/src/capture.rs:387-390` 的 ID 使用枚举序号。
- **用户影响：** 扩展屏热插拔、主屏变化后，新屏不可选，序号变更还可能选择到不同输出。
- **方案：** 监听/检测拓扑变化，用稳定输出身份而非临时索引；活动屏移除时明确回退；旧拓扑下的输入不落入新输出。
- **验收：** 插拔、旋转、主屏切换、100%～200% DPI、负坐标屏幕和睡眠恢复，列表、画面、坐标同步且错误选择可解释。

- [x] 共同协议合同切片（未端到端交付）：VideoConfig 在 stream_id 后发布非零 input_epoch:u64；InputEnvelope 在 timestamp_us 后携带非零 stream_id/input_epoch，InputContext 为不可变 Copy/Hash 标识。input codec 统一前缀 `[0, protocol_version:u16 LE]`，拒绝旧布局、截断、错版本、零上下文与尾字节；各 Postcard 解码边界采用 exact 解码，防旧功能键混淆为新鼠标输入。协议仍为 13。Rust protocol 49 项通过，`check --workspace --all-targets --locked` 通过。主题 `feat(DL2-013): bind input wire to explicit stream and geometry epochs`。
- [x] Windows 共享 Rust API 来源切片 `feat(DL2-013): expose the captured-context runtime input bridge`：准确提取 Apple 已提交 `9ad7154b365de875d725188cfa091f5c21da0b5a` 的 ControllerRuntime::send_input_with_context(&self, InputContext, InputEvent)、必要 import、旧 send_input 明确拒绝及 encrypted controller_runtime 回归；未引入 C/Swift/worker 队列依赖。唯一必要前置为 `bc6c0a62` 的共享合同。`python scripts/run-windows-cargo.py test -p desklink-ffi --test controller_runtime --locked` 5 通过，包含真实加密输入传输；不表示主机/呈现/生产者整链已接通。
- [x] 独立发送所有权切片 `feat(DL2-013): share an ordered input sender outside runtime event polling`：ControllerInputSender/input_sender 与 Runtime 共用同一发送实现及序号，输入专用锁覆盖完整 wire 写入；共享加密锁在网络等待前释放，热路径不逐包 clone Arc。FFI lib 12 通过、真实 encrypted controller_runtime 6 通过；36 帧含 32 并发 sender clone 严格 1..36，原 context 保留、零 context 不发包/不消耗序号。完整 FFI 尚有 2 个旧 Host 零 input_epoch 配置测试失败，保留为生产者未接通证据，不算全通过。
- **工程合同阶段历史范围：** 工程来源 `5501aad9f7d160f751048036ac2a14cf0a315680` 当时只有 wire/codec 合同，host 分配/映射锁及输入生产者仍用显式无效零 context/epoch，并被 codec 拒绝；这不是当前 epoch 兜底。该阶段“尚在接线/不可交付”的记录保留其来源身份，不能覆盖下列 J 的非零 Windows producer 与 A 的完整 FFI/Apple 接线。当前候选仍须联合复跑、补 Apple 旧 input callback receipt 后继修复，并分别验收 Windows 实际 WebView、Apple presented 与真实拓扑/输入，不整体关闭 013。

- [x] Windows 生产者切片 `feat(DL2-013): bind Windows input to painted full-width contexts`：所有 context/VideoConfig 身份 IPC 使用规范十进制字符串，Rust 内部保留完整 u64；覆盖 2^53 前后、u64::MAX 与非法格式拒绝。DOM 事件/pending pointer/滚轮/文字/IME/快捷键/粘贴/释放均固定原 pair；精确 frame/config/channel token 在 drawImage 成功后的可见 rAF 屏障才激活，同 epoch 画质变更保留 DOM 与 held keys。稳态不重复排激活 rAF，输入热路径不读布局。Native 仅持一个输入 future，主循环继续配置/媒体/取消；ACK 粘贴至 Ctrl+V 完成前保持 busy，旧 epoch 后半段局部取消而不拆正在写的可靠帧。Bun 1.3.14 全套 474 通过、构建通过；管理库 135 通过/1 签名资产忽略，Windows 库 113 通过/1 交互桌面忽略。含既有智能粘贴完成/失败后可再次操作回归；不冒称硬件呈现或整链已可交付。

- [x] **已实现并自动验证（`fix(DL2-013): preserve stable monitor identities across enumeration`）：** 使用EnumDisplayDevicesW监视器接口路径稳定映射，进程内最多1024身份且不复用ID；枚举重排/主屏变化不再使旧ID指向另一输出。GDI主屏保留实际descriptor ID；身份不可用/歧义时安全失败。
- **验证：** `python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked capture::identity_tests` 3通过，覆盖重排、拔插重现、大小写归一和注册表耗尽；原生API编译通过。

- [x] **连接内拓扑切片（`fix(DL2-013): publish live display topology with coordinated input mapping`）：** 每秒元数据枚举，稳定ID排序下相同拓扑不重建；活动输出移除明确回退主屏/首屏。准备捕获/编码器并复核OS快照后，以同一锁发布列表/活动屏/坐标/代数，清空旧编码队列并请求IDR；watch推送列表。枚举/驱动失败保留旧发布状态、暂停输入并有界重试。注入持映射锁，跨发布等待的非释放事件丢弃。
- **验证：** Windows lib 128通过/2忽略，含7拓扑和4注入/发布帮助器回归；严格lib+tests Clippy通过；补充编码器准备后OS快照复核后再跑 `--lib topology --locked` 11通过。所有输入用记录/阻塞合成后端，无真实注入或远控。


- [x] **Host严格绑定（`fix(DL2-013): authorize host input with published geometry epochs`）：** 全宽非零随机起点+checked进程共享分配，真实VideoConfig发布；映射锁内先匹配完整context再处理所有事件，旧release不能清除新按键。共享injector统一锁顺序完成旧状态释放/epoch/映射协调，受阻保持暂停重试；纯编码与列表元数据不换epoch。固定32项frame/config→采集context绑定拒绝异步旧输出被贴新epoch，peer退出主动清理。
- **验证：** `test -p desklink-windows --lib --test runtime_contract --locked` 170库通过/2忽略+11契约通过，lib+tests严格Clippy通过。真实生产pipeline配置编码/解码后取token，记录后端覆盖旧移动/Unicode/release、A→B→A、新Noise/进程相同stream、纯编码保持、旧MFT输出、RNG失败/零值/u64耗尽。规则/复跑见 `docs/windows-host-input-epochs.md`；无真实注入/共享。

- [x] **配置版本边界（`fix(DL2-013): prevent encoder configuration version reuse`）：** 编码器三个配置推进点均在替换后端/设置前checked预留非零后继，u32耗尽返回ConfigVersionExhausted，不wrap或重置1；提供实际config_version读取供Host原子配对校验。encoder 20项通过，边界失败后COM后端/设置/帧计数/pending引用不变，lib严格Clippy通过。

- [x] **真实配置配对（`fix(DL2-013): pair input epoch publication with actual encoder versions`）：** Coordinator所有发布显式读取encoder.config_version；几何/有效性边界在分配epoch前验证真实新版本大于epoch出生版本，metadata保持anchor。HostPipeline拒绝倒退/同版本换epoch或尺寸，正确新版本仍需IDR。保留ControllerRuntime原有相同身份字段变更拒绝，不冒称已复现旧帧误激活。配置u32耗尽在各捕获重配入口退出peer，不做无穷重试/复用1。
- **验证：** 174库通过/2忽略+11契约通过，lib+tests严格Clippy通过；同尺寸/同SPS选屏与虚拟原点、旧MFT输出回归均从实际SoftwareOnly编码器重配/getter取得版本并由真实pipeline发布token，非修改常量fixture求绿。

- [x] **输入发送类型静态检查跟进（`refactor(DL2-013): name the owned input send future type`）：** 为既有 owned future 提取等价类型别名，消除 `clippy::type_complexity`，不改变发送、锁、取消或队列语义。管理库 `test -p desklink-windows-ui --lib --locked` 135通过/1签名资产忽略；本包 `clippy -p desklink-windows-ui --lib --tests --no-deps --locked -- -D warnings` 通过。基线依赖的021 cancellation lint已由媒体来源提交修复，跨包严格检查由联合门禁复跑。

- [x] **Windows 联合自动验证（保留来源历史的产品／媒体 merge）：** 产品 `16594c4` + 独立013类型别名 `27fec75` 与媒体 `65c70fd`（含89版号配对及后继044/045/046/021初始化取消修复）逐段整合；真实 SoftwareOnly BGRA→HostPipeline→localhost QUIC/Noise→ControllerRuntime captured sender→Host原子mapping→RecordingBackend的2项回归通过。覆盖同尺寸同SPS新版本/epoch、纯编码保持held、新Noise拒绝旧context与旧release；精确旧plaintext重加密另为packet级检查。最终Rust脚本计数634通过/2个未接入完整Apple ABI/MacHost失败/19忽略；Windows283、管理库135、共享178通过，FFI38通过；八包严格Clippy无allow/无no-deps通过，Bun1.3.14 474通过与build通过。477源码摘要运行前后不变，详见 `docs/windows-joint-integration.md` 与外部最终handoff清单。主013、真实WebView/双机/硬件及MacHost接线仍未完成。

### DL2-044 · P1 · 可选光标数据报降级与发送诊断

- [ ] 完成可选光标降级及用户可见诊断接入验收。
- **源码复核修正：** 锁定Quinn 0.11.11/proto 0.11.16的send_datagram在队列压力下逐出旧报文并返回Ok，不存在公开QueueFull；TransportError::Datagram只覆盖该发送API的UnsupportedByPeer/Disabled/TooLarge。不能用连续失败预算模拟队列压力，也不能把Ok或发送错误次数当作送达/丢包统计。
- [x] **生产策略（`fix(DL2-044): disable unsupported cursor datagrams without ending peers`）：** 首个Datagram错误仅停用当前peer的可选光标，后续不采样、不加密、不重发，future保持pending等待主peer取消。连接/代数/身份/preflight错误仍立即传播。每个peer独立状态，一次固定capability_or_size原因；send_attempts/send_errors/disabled字段只表示本地操作。
- [x] **有界诊断接线：** HostSupervisor/HostRuntime提供with_media_diagnostics_observer，构造时初始化进程唯一32槽队列/工作线程；热路径try_send、不做同步文件或HTTP，回调异常不杀接收器。DiagnosticEvent::HostMedia提供固定JSON字段映射；队列拒绝数与网络丢包明确分离。未绑定observer时仅后台stderr回退，不视为用户导出/服务端可见性已交付。接口见docs/windows-host-media-diagnostics.md。
- **验证：** 光标10项（含真实生产循环停止采样但不完成peer future）+诊断分发2项通过；真实127.0.0.1传输20项通过，包括对端禁用、低于应用preflight上限的TooLarge、终态/代数检查及1字节Quinn发送缓冲下1000次Ok，之后同连接控制/输入可用。Windows+transport lib/tests严格Clippy通过。
- [x] **GUI 日志与报告接线（`fix(DL2-044): persist cursor diagnostics in desktop reports`）：** 普通Host与配对Host均注册typed observer，复用各worker的DiagnosticLog；既有后台有界dispatcher执行落盘，原stream保持为历史记录，不更新当前GUI状态。实际callback→临时日志/sync_data→sanitized history→生产合并/报告builder回归通过；同services `b1118ad465703493255123e23b6f522662c3d603` 原样fixture的4个case逐字段/类型比对通过（仅忽略动态timestamp，u64溢出仍是服务拒绝的negative）。Windows lib218通过/2忽略、管理库137通过/1签名资产忽略，两包lib+tests严格Clippy通过。云上传源码保持冻结基线，不吞事件；服务历史尚未合入本GUI分支，整体验签/存储另验。
- [ ] 将已提交服务合同与GUI接线纳入最终整体候选并验证真实接收/存储；真实GUI导出操作、网络长稳与实际双机鼠标体验另验，不从localhost推定丢包率。

- [x] **044/045共同u64精度修复（独立切片）：** 先以合法配置→actual日志→Rust原始body/hash/Ed25519 SignedRequest→固定旧服务validator复现：safe/zero-counter对照通过，2^53/2^53+1/MAX stream及MAX counter被400；原v1“overflow negative”只是旧服务拒绝现象，不代表合法u64本身非法。七事件18个真实u64键仅在诊断JSON边界改规范十进制string，typed observer仍u64；旧Rust日志按event+key精确无损规范化后重新序列化/hash/sign，不复用旧签名，不过滤。选中批次known非法值整批Encoding；unknown仍交服务固定schema。唯一producer fixture含6当前+6历史签名请求与实际字段表/source指纹。Windows `--lib`226通过/2忽略，管理库141通过/1忽略，两包lib+tests严格Clippy通过。细节与未验项见 `docs/windows-diagnostic-u64-contract.md`。

### DL2-045 · P1 · DirectLan accept 分类复核与终态诊断

- [ ] 区分监听终态并使禁用原因可观测。
- **源码复核修正：** a905b88与本分支 `DirectLanEndpoint::accept` 都返回Option<Result>；握手失败/超时是Some(Err)，Host已continue。`endpoint.accept().await?`只传播None监听终态，不能把“任何Err永久禁用”当作已确认缺陷；实际剩余缺项是None后的明确降级诊断。
- [x] **已有正确行为的真实回归（`test(DL2-045): distinguish transient direct handshakes from listener shutdown`）：** 空truststore触发真实127.0.0.1 QUIC证书握手失败，确认Some(Err)，同一listener随后使用生产配置+Noise会话成功认证probe并收发datagram；显式endpoint关闭才在100ms内返回None（重复等待一致）。probe 14项通过，transport lib+tests严格Clippy通过。
- [x] **终态生产处理（`fix(DL2-045): retire direct paths when the listener terminates`）：** accept返回None时一次性take本代session、abort探测，路径锁内同时禁用后续安装并取走活动Arc，锁外关闭、请求IDR；继续控制/配置/输入消费。迟到探测在同一路径锁内检查禁用标记，不能在清理后复活路径。固定DirectListenerStopped/ListenerClosed原因通过044后台observer分发，DiagnosticEvent映射host_direct_listener_stopped；session=None的accept future保持pending防刷屏。
- **验证：** Windows新增2项通过：真实认证双路径的活动Arc清理/IDR、已完成probe但尚未安装的迟到连接被关闭、诊断只一次与禁用后accept pending；固定日志字段测试。原有Some(Err)continue与真实endpoint.close→None边界另由probe回归证明，不冒称复现原本不存在的“握手错误禁用”缺陷。
- [x] **GUI 日志与报告接线（`fix(DL2-045): retain listener termination in desktop reports`）：** 044已注册的普通/配对Host observer同时记录typed DirectListenerStopped，复用原日志与报告链；固定reason与retired_active两值均保留，不推断中继随后已恢复。新增同services `bf5d7eb27f3f41be59ac317b63d7e93d10f38066` 原样fixture四case→实际callback/临时日志/sync_data→生产合并/报告builder的七字段/类型回归；仅忽略动态timestamp，完整保留u64，overflow仍是服务negative。最终管理库`--lib`138通过/1签名资产忽略，两包lib+tests严格Clippy通过；044已验证的Windows`--lib`218通过/2忽略源码保持不变，不混同原joint的283/17（lib+integration范围）。原始日志与源码前后指纹见 `E:/desk-link/.worktrees/dl2-targets/windows-diagnostics/validation/`。
- [ ] 将GUI接线与已提交服务合同纳入最终整体候选，验证真实验签/接收/存储及实际GUI导出；真实监听故障、设备与长稳另验。服务历史尚未合入此GUI分支，不把producer/报告builder自动证据当成整套验收。

- [x] **共同u64数值精度切片：** 与044同一独立提交；DirectListenerStopped stream与所有同行Lifecycle/Controller u64字段都无损JSON字符串，GUI诊断报告不混淆相邻大stream。旧实际签名body保留，新日志与旧日志升级请求重算hash/sign；同一producer fixture供services消费，原生observer/原因/retired_active与输入wire未改。完整服务闭环尚未在本分支合入验收，不能以producer测试总体打勾。

### DL2-046 · P1 · 出站探测生命周期与活动直连仲裁

- [x] 探测任务纳入会话取消树；活动路径按同一peer槽位和连接分配身份仲裁。
- **审查/修复边界：** 基线出站spawn脱离生命周期与无条件覆盖存在；011的JoinSet已接管取消，本条补实际回归及剩余抢占/旧egress清空新路径问题，不重复冒称取消是本笔才实现。
- **修复（`fix(DL2-046): retain active direct paths and retire only matching connections`）：** 入/出站认证完成统一安装，健康活动连接保留、关闭连接才替换，相同Arc不误关；健康路径期间拒绝冗余offer。旧发送失败只移除同一Arc实例，不能按候选ID清掉后继；拒绝/退役连接在释放槽锁后关闭。
- **验证：** 6个固定槽位用例+3个真实127.0.0.1 QUIC/Noise回归通过，lib+tests严格Clippy通过。两条竞争路径共用同Noise会话，晚出站被关闭后原入站仍双向传输加密VideoDatagram；关闭旧路径可替换且旧egress不能清新路径；真实connect_shared响应等待时Drop JoinSet后250ms内guard/permit释放、crypto锁可取。未建立实际远控/输入会话；011的双机p95/p99仍单列。

### DL2-047 · P1 · 智能粘贴 busy 状态在成功路径不复位，远端键盘被静默吞掉

- [ ] 为 smartPasteBusy 提供与完成信号解耦的复位保证。
- **证据 / 类型：源码缺陷。** `apps/windows-ui/src/controller.ts:3452-3468` 仅 catch 分支复位 busy；成功路径依赖 `clipboard` 完成信号（`1637-1638`），而该信号经 `1263-1276` 的 `generation === channelGeneration` 守卫——粘贴 invoke 成功后若通道代数推进（取消、重连），旧信号被丢弃且新连接不再触发 `1506` 行的复位；此后 `3355-3357` 的 `if (smartPasteBusy) { event.preventDefault(); return; }` 静默吞掉全部远端 keydown，直到下次断连。
- **用户影响：** 一次粘贴叠加代数切换后远端键盘"失灵"，界面无任何提示。
- **方案：** try/finally 复位加有界超时兜底；代数切换时主动清 busy；粘贴状态与发起时的通道代数绑定。
- **验收：** 粘贴进行中取消/重连后键盘立即可用；completed/failed 正常复位；不产生重复粘贴。

### DL2-048 · P1 · 会话密钥由发起方单方选定且未纳入握手签名

- [ ] 会话密钥改为双方贡献派生，并把密钥材料纳入签名 transcript。
- **证据 / 类型：源码缺陷（加密设计）。** `crates/desklink-crypto/src/noise.rs:288-302` responder 将 initiator 载荷尾部的 32 字节原样作为 session_key，不做任何校验；`708-721` 的 Ed25519 签名只覆盖 `encode_auth_payload`（device_id、verify_key、transcript），不含 key 字节。
- **影响：** 会话密钥的机密性完全取决于发起方（控制端）的随机数质量与善意：被篡改或恶意的控制端可以把密钥固定为与第三方共享的值，而 `PacketCipher::new`（`noise.rs:461-477`）用 `BLAKE2s(domain‖key‖dir‖lane)` 确定性派生全部 lane 密钥，路径上的窃听者一旦得知该值即可被动解密主机发出的视频、音频与输入密文——把机密性从"连接双方"扩大到"任何拿到该值的人"。退化取值（如全零）也不会被拒绝。
- **方案：** 双方各贡献随机数，经 HKDF(Noise handshake secret, c1‖c2) 派生会话密钥，任一方都无法单方决定；至少应将 key 字节纳入签名并拒绝退化取值。随本阶段协议版本一并升级，与 DL2-024/025 的协议变更统筹。
- **验收：** 任一方无法单方决定会话密钥；篡改密钥材料导致握手失败；新增恶意发起方/密钥固定用例的回归测试与协议兼容矩阵。

## 4. 性能与数据通道

以下 Apple/FFI 013 条目按来源提交顺序保留阶段记录；其中“本分支”指对应 Apple 来源，早期生产隔离点及测试计数不代表当前 J+A+N 候选。Apple 最终来源为上表 A（`f954a68153168a1ded048ddf93eb9c565684d096`），候选实际联合复跑尚未完成。

- [x] 共同协议合同切片（未端到端交付）：VideoConfig 在 stream_id 后发布非零 input_epoch:u64；InputEnvelope 在 timestamp_us 后携带非零 stream_id/input_epoch，InputContext 为不可变 Copy/Hash 标识。input codec 统一前缀 `[0, protocol_version:u16 LE]`，拒绝旧布局、截断、错版本、零上下文与尾字节；各 Postcard 解码边界采用 exact 解码，防旧功能键混淆为新鼠标输入。协议仍为 13。Rust protocol 49 项通过，`check --workspace --all-targets --locked` 通过。主题 `feat(DL2-013): bind input wire to explicit stream and geometry epochs`。
- [ ] **当前候选剩余验收：** J 的 Windows 非零 host/producer 与 A 的 FFI/Apple 接线合入后，仍须候选跨包复跑、Apple 旧 input callback receipt 后继修复及实际 WebView / presented / 拓扑和输入验收。合同阶段的零 context 隔离记录见 013 历史范围，不据此认定当前候选生产者仍为零，也不整体关闭 013。

- [x] Controller/FFI 输入切片：`ControllerRuntime::send_input_with_context(context: InputContext, event: InputEvent)` 严格校验并序列化原 context；旧无上下文方法保持拒绝。FFI DesklinkInput/Event 贯通 epoch，有界队列、合成 release 按 context 保存；新增 `desklink_release_all_for_context`，旧 epoch 的同名键释放不会清掉新 epoch ledger，空 ledger 回收且最多跟踪 8 个 context。生成 C 头与实际 C/Rust 布局一致。Rust 20 单元+7 ABI+2布局+5 Controller loopback共34项通过。主题 `feat(DL2-013): preserve captured input context across FFI queues and releases`。

- [x] FFI Mac Host 切片：随机种子进程共享非零 epoch 分配器，溢出失败；同 stream/尺寸编码重配保留 epoch，换流/尺寸发布前可靠清键，输入仅匹配已发布 context。旧 context 在时钟/过期检查前丢弃；满队列保留终态清理容量，清键/keyup 无法可靠入队则安全终止。Rust 23 单元 + 14 真实本地 Host loopback 共37项通过，覆盖配置前不注入、旧keyup/过期旧keyup/鼠标丢弃、换几何、饱和与取消。主题 `fix(DL2-013): enforce published input epochs on the FFI host`。
- [ ] FFI Mac Host 当前只可绑定 stream 与编码尺寸；完整源显示器/捕获拓扑身份和真实 Mac 注入验收仍缺，不能据此整体关闭 013。

- [x] Controller 同 epoch 改编码尺寸视为配置合同错误，拒绝保留可输入的旧几何；同几何画质重配保持 epoch，epoch 身份不按数值排序。新增真实运行 Rust guard 回归；本分支 FFI 全57项与 Clippy all-targets `-D warnings` 通过。主题 `fix(DL2-013): reject geometry changes that reuse an input epoch`。

- [x] Apple 呈现与输入完整代码切片：VideoConfig epoch 绑定解码/呈现令牌；Metal drawable 真正呈现且 presentedTime>0 才启用新输入 surface，迟到回调校验 runtime/stream/config/epoch。Mac/iOS 按键、IME、手势和延迟长按捕获原 context，纯画质重配同 target 不释放；重连在 FFI callback 同步退役旧流，支持新认证会话较小 epoch/stream，旧 callback 不清新 runtime。Python DL2 合同74项通过（其中呈现10、输入捕获9）；Swift模型/事件测试已补但本Windows环境未执行。主题 `fix(DL2-013): bind Apple input lifetimes to actual frame presentation`。
- [ ] **Apple 来源与候选剩余验收：** 此早期 Apple 切片的 Windows Host/前端隔离点由候选保留的 J 生产接线承接，不能外推为候选仍未接通；FFI Mac Host 完整捕获拓扑身份、Swift 旧 input callback receipt 后继修复、Swift6编译、真机呈现/输入/跨设备验收仍缺。以上是代码/本地合成证明，不是013整体验收。

- [x] 新认证会话与熵源补充：每个新 Noise 即使 stream/尺寸复用也取得不同随机起点序列中的 context；RNG 使用 try_fill_bytes，错误/零值/MAX 初始化失败均明确拒绝并缓存，无panic/回落1。新增合成熵源边界与真实 relay generation 切换回归；修复夹具无截止等待后新Noise回环0.70s通过，本分支完整FFI 62项通过。主题 `fix(DL2-013): fail closed on epoch entropy errors and verify reconnect identity`。

- [x] FFI Host 发布前也校验同 stream 的尺寸变化必须提高真实 config_version；不在 Rust 层重写 Swift 帧版本。无效旧版本几何配置明确终止并保留清键，Controller原有same-version变化拒绝保持。新增本地回环与几何policy回归各1项通过。主题 `fix(DL2-013): require new frame versions for changed host geometry`。

- [x] 复用产品组有序 input sender：来源 `3ea72dc7f5a66d4cd96c197c0aadb7603cee2436`（其最小前置为bc合同+b473桥接，本分支已有等价9ad）；Runtime与clone共用序号并持input专用锁至可靠写完成，secure锁不跨网络await。此依赖的本分支接入回归将在后续worker切片记录，不能引用来源分支结果冒充当前证明。
- [x] 共享 sender 接入回归：修正本分支 direct-silence 夹具的共享序号器构造；`python scripts/run-windows-cargo.py test -p desklink-ffi --lib --tests --locked` 实跑 71 项通过，包含 sender/runtime 加密序号与原始 context、直连静默及 C ABI 布局。此项仅证明 sender 接口，worker 并发事件轮询仍待后续切片。

- [x] Host 配置版本跟进：保存完整已发布 VideoConfig；同 stream 任何版本回退在发布前拒绝，同版本的解码配置（含 SPS/PPS、尺寸、epoch、codec/protocol）必须完全一致；幂等重试不重发配置或把后续 delta 误标 CONFIG，新 stream 仍可从 version 1 开始。旧 access unit 不重贴新版本。`test -p desklink-ffi --test host_runtime --locked` 实跑 23 项通过（新增 4 个真实本地回环）；完整配置比较单测从当前编译的测试二进制运行通过。日志位于本机 apple-ffi target/evidence/host-version-*.log；Swift producer 最后投递归属另有后续切片，不能以此项替代。主题 `fix(DL2-013): reject regressed or mutated host decoder configurations`。

- [x] 真实背压夹具所需的传输构造入口：QuicFlowControlLimits 显式限制 send / connection receive / stream receive，非零、最多 512 MiB 且 stream 不超过 connection；默认行为保持原值。client connect 应用限值后统一 keepalive / dead timeout，relay 新 bind_with_transport 保留传入窗口并统一生命周期超时。原来只设置 TLS client/server config 的窗口会被构造器覆盖，不能据此声称制造了背压。`test -p desklink-transport -p desklink-relay --lib --tests --locked` 实跑 77 项通过；最终端点背压证明见后续 worker 切片。主题 `feat(DL2-013): expose explicit bounded QUIC flow-control tuning`。

- [x] Host 原生生产者归属：每次认证 Noise 的 Connected 固定生成非零、全局随机起点后 checked 推进的 media_generation；Connected 先保留 required event 槽、发布 owner / state，再投递原始令牌，断会话立即退役。scoped config / access-unit / cursor C 入口在入队和所有实际出队阶段校验原 owner，旧 producer 或已入队旧媒体只拒绝 / 丢弃，不关闭后继会话；StaleGeneration=5，事件尾字段与双头、C ABI 布局同步。Rust legacy 同步媒体入队也保留当时 owner，异步 producer 必须使用 scoped 入口。Host runtime 26 项、Host ABI 5 项、双头与全部 public struct 布局 2 项通过；另实际首连→换 Noise→审批 / 协商 / Connected 注入旧队列的 lib 回环及不可改写 Connected callback 快照通过。完整 source display / topology 仍未覆盖。主题 `fix(DL2-013): bind host media producers to their authenticated Noise session`。

- [x] Controller worker 并发：clone sender 的 owned future 与持久栈 pin next_event 同时轮询，命令 / send 完成不会重建已取可靠消息且等待 crypto 的 reader；输入保持原 context，epoch 只过滤未首次 poll 的发送 / 未出队旧输入，已开始可靠写不截帧。ReleaseAll 有界 1024，溢出 fail-closed；取消独立于满队列，结果分支再检查用户取消，避免错误重连。Control 专用锁保证 seal / write 顺序，secure 锁不跨网络；包括自动关键帧 / Direct 回答在内的控制发送共用一秒预算，截止关闭原会话。6 项并发回归实跑通过（4 项真实 QUIC / Noise，2 项边界策略）：真实双跳背压时配置 / 帧继续、旧输入过滤且原在途帧完整、长 release 仍收媒体且取消有界、已 dequeue 可靠消息不丢、控制背压截止与连接关闭。测试端点使用明确 2 KiB flight 窗口，不靠无效 TLS config 参数或 mock 背压。主题 `fix(DL2-013): keep receiving media while ordered input writes are pending`。

- [x] Mac Host 最后投递归属：每 capture attempt 独立 source / encoder，所有配置 / 帧 / 错误闭包绑定 immutable runtime / connection receipt / native media_generation / stream；native callback 同步退役 gate，主线程 receipt 校验后最终 scoped C admission。失败 / 取消的 stream 号不复用；新 start 等旧 start / stop cleanup barrier，旧 catch / stop 不能影响后继 encoder。捕获错误只退役自身并保留可重试告警，旧错误不全局停止新 Noise。新增 8 项 portable 合同通过；10 个合成 Swift 生命周期 / 调度用例未运行，不能代替实际 ScreenCaptureKit / VideoToolbox / 权限 / GPU 验收。主题 `fix(DL2-013): retire captured host resources before a successor can start`。
- [x] 本地组合门禁：`test -p desklink-ffi --lib --tests --locked` 实跑 88 passed / 0 failed / 0 ignored；`python -m unittest discover -s scripts/tests -p "test_*.py"` 实跑 201 passed；双 C 头生成检查、fmt / diff 检查通过。日志在 `E:/desk-link/.worktrees/dl2-targets/apple-ffi/evidence/`，最终不可变 HEAD 与日志哈希由交接证据绑定。Windows 本分支 producer 的 epoch=0 隔离占位仍待整合 Windows 联合快照；不能将 FFI 套通过宣称整包可交付。

- [x] P2 排队输入的最终注入 admission 跟进（独立于冻结 f954）：修复旧 A.input 已排 MainActor、native 已观察 Connecting / B Connected、Swift state 尚显示 connected 时旧点击仍被注入的缺口。HostEvent 保留原 fixed Noise media_generation 与完整 InputEnvelope；C callback 传原 grant、stream_id、input_epoch，双头与 ABI 尾字段同步，不从 current 重新贴标签。native input 入队时固定连接 receipt / capture owner / 原 context，最后注入必须同时满足 captureRunning、当前 capture / runtime 身份及同一 native admission，检查和本地同步 backend 调用共享短 gate 临界区。ReleaseAll 与用户取消保持清理语义；不持 crypto / 网络锁跨注入。主题 `fix(DL2-013): preserve native host input admission until final injection`。
- [x] 本条 Windows 实跑：FFI lib/tests 91 passed（新增 2 个真实 QUIC / Noise input 归属回环、1 个实际 C callback 快照；含全部 C struct sizeof / align / offset）；Python 全套 208 passed（新增 7 个输入 admission 源码合同）；workspace all-targets check、严格 FFI all-targets Clippy、双头生成、fmt / diff 检查通过。证据在 `E:/desk-link/.worktrees/dl2-targets/apple-ffi/evidence/host-input-admission/`。
- [ ] 本条 Apple 行为验收：新增 6 个不调用真实系统注入的 Swift recording backend / native callback 调度用例，覆盖两个 UI 滞后交错、合法 B 输入、原 owner/context 不重贴、退役 capture、release / stop；Windows 无 Swift/Xcode，尚未编译或执行。真实 Mac 输入与权限验收、未来同 stream 完整源拓扑 / epoch 的权威映射仍不在本条已证范围，不整体关闭 013。

### DL2-014 · P1 · 建立第二阶段统一性能基线

- [ ] 先记录真实性能，再按阶段预算优化，保留前后同条件对照。
- **证据 / 类型：工程缺口。** 现有 `apps/windows/src/diagnostics.rs:516` 已记录首帧、FPS、帧间隔、drop 等，`apps/windows-ui/src/video-render-timing.ts` 有滚动统计；本次没有统一的双机 CPU/GPU/内存、输入到显示、各阶段队列年龄与复制量对照结果。现有诊断不等于已经证明端到端性能最优。
- **方案：** 将 capture → convert → encode → queue → transport → assemble → decode → display 与 input → inject 分段；关联同一帧/会话代数，使用单调时钟测本机阶段；跨机测量校准时钟误差，或用外部画面/回显方法，不能直接相减两个系统时钟。
- **验收：** 输出可复跑的硬件/网络/画面矩阵与 p50/p95/p99，记录分配率、copy bytes、功耗/温度、队列年龄、音画延迟和长稳斜率；每次性能变更附同机对照，禁止只报平均 FPS。
- **依赖：** 为 DL2-015～024 提供量化依据；不能为了追求单项 FPS 牺牲输入、文字清晰度或恢复正确性。

- [x] **已实现并自动验证（`perf(DL2-014): add reproducible offline media stage baseline`）：** 编码器固定单样本记录本机单调时钟convert/MFT/输出驻留与显式复制字节；离线合成基线命令见 `docs/windows-media-baseline.md`，输出p50/p95/p99、运行前后源码指纹，未测GPU/网络/端到端字段为null。
- **验证：** 经 `scripts/run-windows-cargo.py` 执行 `test -p desklink-windows --test encoder_contract --example media_baseline --locked`（11+1通过，1个既有人工基准忽略）；`clippy -p desklink-windows --lib --example media_baseline --locked -- -D warnings`通过；Python编译与64×64/3帧工具烟测通过。并行编辑期间报告正确标记源码变化，烟测不是稳定性能结论。
- [ ] **仍待完整基线：** GPU/双机、队列及端到端、分配/功耗/温度/长稳矩阵与同条件前后实测；本切片不宣称性能收益。

- [x] **缓冲输出归因（`fix(DL2-014): distinguish submitted and buffered output frame timings`）：** 固定样本新增output_frame_id，frame_id仍表示当前输入；输出驻留使用实际匹配的旧PendingFrame，避免异步缓冲时把上一帧时间归到本帧。encoder 15项通过，含input12/output11事件fixture及实际软件64×64编码输出ID核对；lib严格Clippy通过。

### DL2-015 · P1 · 将 Windows 捕获与颜色转换尽量留在 GPU

- [ ] 重构 texture 生命周期，减少每帧 CPU 回读、逐像素转换与临时分配。
- **证据 / 类型：性能结构。** `apps/windows/src/capture.rs:505-521,688-759` 每帧创建 staging texture、CopyResource/Map、分配并复制 BGRA；`apps/windows/src/encoder.rs:122-261,496-501` 再分配 NV12 并 CPU 转色/缩放。
- **方案：** 进程拥有的 GPU texture 池、GPU 缩放/转色与明确 fence/所有权；编码前尽量不回 CPU。必须回退时复用 staging、输出及转换 buffer。不能直接跨 ReleaseFrame 保留桌面复制 surface 并假定仍有效。
- **验收：** 1080p/1440p、集显/独显/混合 GPU 下比较复制字节、分配、CPU/GPU 时间及帧龄；GPU 常驻路径故障可降级且不破坏输入。收益待实测。
- **依据：** [Microsoft 的 Direct3D-aware MFT 说明](https://learn.microsoft.com/en-us/windows/win32/medfound/direct3d-aware-mfts)给出了 surface 输入和 D3D manager 接口；是否能用于具体编码器必须实测能力。

- [x] **已实现并自动验证（`perf(DL2-015): reuse owned staging textures for desktop readback`）：** 捕获进程持有staging/context缓存，以D3D device身份和完整texture布局失效；Map/复制/Unmap在ReleaseFrame前完成，不保留失效桌面surface。
- **验证：** `test -p desklink-windows --lib --locked capture::` 10通过、2忽略；3个WARP合成用例验证复用后新像素、尺寸/device失效、row pitch/padding；显式dev WARP基准1通过、staging创建60→1。命令与release复跑规范见 `docs/windows-capture-readback-benchmark.md`。上述是软件D3D工具/正确性验证，不代表真实GPU收益；CPU回读字节不变。
- [ ] **仍待整链/完整硬件验收：** GPU surface编码链、CPU捕获输出buffer池，以及1080p/1440p集显/独显/混合GPU时间/复制/分配/帧龄对照；owned池/缩放转色进展见第二波完成项。

- [x] **转换buffer切片（`perf(DL2-015): reuse encoder NV12 scratch buffers`）：** 生产编码器复用NV12 Vec，新增checked `convert_to_nv12_into`，保留原分配API；1:1转换不再为x/y索引建临时Vec。基线工具支持 `--reuse-buffer` 并记录增长次数，GPU与CPU输入复制未改变。
- **验证：** 新buffer 3项、原encoder_contract 11项、backend 3项、percentile 1项通过，严格Clippy通过；显式release `--test encoder_buffers conversion_buffer_reuse_release_baseline -- --ignored --nocapture` 1通过。30样本/API、5次预热，同进程1080p分配/复用p50=4.8271/4.3698ms、p95=4.9710/4.4706ms、p99=5.0626/4.8022ms；1440p p50=8.6875/7.8823ms、p95=8.9543/8.0388ms、p99=9.0792/8.2324ms；预热后buffer增长0。仅为本机CPU转换API对照，非旧commit/真实GPU/端到端收益。

- [x] **第二波owned GPU池与转色（`feat(DL2-015): add bounded owned GPU surfaces and NV12 processing`）：** GPU-only BGRA复制及VideoProcessor缩放/BT709 limited→NV12，普通路径无Map；Capture提供新CPU/GPU入口且保留旧CPU API。复制EVENT完成后才交付并ReleaseFrame；Clone共享lease，NV12独占lease供后续TrackedSample持有；超时/未完成fence隔离槽位。全进程64槽/512MiB逻辑图像字节预算跨pool换代计数，不能用丢旧pool逃逸预算。失败可对同一Acquire源完成CPU读回，新CPU帧清旧GPUcache；确认Idle后才允许缓存owned图像恢复IDR。
- **验证：** 基于65c70fd归档加本切片4源码文件的独立快照（隔离并行016 WIP）原生GPU测试7通过：3个纯内存/状态用例+4个本机RTX4070TiSUPER合成纹理用例，覆盖source销毁、Clone占用/池满、709limited缩放色值、NV12跨pool存活、invalid/pending EVENT禁止复用、跨代64槽上限；独立快照完整lib 204通过/6忽略、lib/tests严格Clippy及定向格式检查通过。没有采集真实桌面、未推定MFT接受surface或实际编码成功。结构/命令/计量边界见docs/windows-gpu-media.md。
- [x] **声明顺序格式跟进（`style(DL2-015): normalize GPU module declaration order`）：** 固定2d79f97补落盘日志时workspace fmt发现lib.rs仅gpu/identity声明排序差异，跟进只调整该顺序；原GPU7项、完整lib204项与Clippy均通过，原失败日志如实保留，不改GPU实现。
- [ ] **后续完整验收仍待：** CPU输出buffer池、集显/独显/混合GPU及真实DXGI/双机/驱动恢复矩阵与性能；runtime GPU选路与MFT surface输入已在016完成项接通，单独owned池切片不作为生产整链或收益证明。

- [x] **非热路径退役清理（`fix(DL2-015): trim retired GPU generations after safe ownership release`）：** Pool销毁先释放自身slots再按同一完成规则trim，捕获切软件时释放旧pool/cache，capture+encoder返回后worker再trim。只回收已无外部owner且EVENT完成的退役槽位；live lease、未完成/无效fence仍计全局预算。MF回调仍只原子归还lease，不做GPU查询/全局清理；late callback后的有界缓存可等后续trim，不冒称立即全部释放或把旧策略称泄漏。
- **验证：** GPU模块8项（含本机合成live lease/incomplete query→End完成后的安全回收）及真实TrackedSample外部COM引用跨encoder/guard销毁仍占用的寿命回归通过，严格lib/tests Clippy通过。resident统计是逻辑槽/图像字节，不是实测VRAM。

- [x] **可复跑GPU计量工具（`perf(DL2-015): add source-bound synthetic GPU stage probes`）：** 新gpu_media_probe及benchmark-windows-gpu.py，分capability/benchmark、显式adapter/input-path/experimental4k；CPU输入不假定软件backend。确定性desktop glyph-like/gradient/panels及滚动，tiles专用于两phase颜色/方向/UV参考；报告requested/actual尺寸/profile/bitrate、真实MFT/active状态、sample ID、pending未drain范围。区分GPU copy/VP输出、CPU读回、fixture上传/clone、texture/query/view创建；GPU时间/driver copy/heap分配未知为null。wrapper逐run及跨repeat绑定完整源码hash/HEAD/clean状态，fallback或不稳定源不作GPU比较依据。4项示例测试、严格example Clippy及Python编译通过。
- [x] **正式本机阶段矩阵（`docs(DL2-015): record the frozen synthetic GPU stage matrix`）：** 固定6a60f45、clean、同一files_sha256，desktop-motion的1080p/1440p/显式4K两输入路径各3×30样本/3预热，18benchmark+1capability native进程全部exit0；实际尺寸、Main18M/4K High40.5M匹配。GPU均NVIDIA surface后端、fallback0、normalCPU读回/NV12 CPU写入/MF输入copy=0；CPU实际同NVIDIA system-memory后端。每run正常GPU query60/views各30，motion fixture另建纹理30，非零分配/零拷贝。数据表和原始JSON/hash路径见docs/windows-gpu-media.md；严格不将返回30输出/lastpending0当drained cohort或FPS。
- [ ] **性能外推仍待：** GPU实际执行时间/driver内部copy/heap分配、质量等效、真实桌面/多GPU/双机/帧龄/功耗/长稳；本机CPU墙钟分阶段数据不外推端到端收益。
- [x] **当前第二波代码/软件收口（干净源97ff7c0）：** 第一波冻结c3后完整merge GPU235七笔，owned BGRA/NV12池、完成fence/lease、全局预算、退役清理及runtime GPU入口在独立候选中实际接通；25软件检查和6条选中GPU命令全部通过，GPU模块8项通过/0忽略。选中总24项含原ignored实际执行，不能与默认软件套件重叠项求和。见[第二波GPU集成记录](docs/wave2-gpu-integration.md)。
- **来源/剩余验收：** 上述M早期CPU路径、G owner计数及6a60f45阶段矩阵保留各自来源，不冒充97ff性能复跑；本次完成代码/软件小项，真实DXGI Acquire/Release、设备恢复、多GPU、同画质性能/帧龄与长稳仍 `[ ]`，015主项不关闭。

### DL2-016 · P1 · 显式选择硬件编码器并完善降级

- [ ] 建立编码后端能力探测、选择、诊断与恢复机制。
- **证据 / 类型：性能结构、能力缺口。** `apps/windows/src/encoder.rs:734-772` 固定 CoCreateInstance CMSH264EncoderMFT；`1053-1074` 创建 MF buffer 并再次复制 NV12。当前没有硬件 MFT 枚举和 GPU surface 输入链。
- **边界：** 可确认没有显式硬件选择链路，不能仅凭此断言所有设备的实际编码都已测定为纯软件。
- **方案：** 枚举并选择合适的异步硬件 MFT，查询实际能力/低延迟参数，处理设备丢失及软件回退；记录真正使用的后端、profile、分辨率、帧率与原因。[MFTEnumEx 官方说明](https://learn.microsoft.com/en-us/windows/win32/api/mfapi/nf-mfapi-mftenumex)区分了同步、异步和硬件类别。
- **验收：** Intel/AMD/NVIDIA 与无可用硬件编码器矩阵；同画质对照 CPU、GPU、帧龄和恢复时间，驱动拒绝参数时能够快速选择可用配置。与 DL2-015 联合实现。

- [x] **已实现并自动验证（`feat(DL2-016): select hardware MFTs with bounded software fallback`）：** 显式MFTEnumEx枚举NV12→H264硬件候选（最多尝试16），async unlock/事件credit授权/100ms进展预算；失败以相同帧在新config_version强制IDR重试软件，后续重配保持软件避免振荡。backend_info及基线报告记录实际后端/候选/稳定原因；见 `docs/windows-encoder-backends.md`。
- **验证：** 2个新增credit/合成硬件故障用例通过，encoder_contract 11通过/1人工项忽略，基线percentile 1通过；lib+example严格Clippy通过。64×64合成软件及prefer-hardware各3帧烟测通过；后者真实枚举1候选、拒绝该配置后软件完成，报告source_unchanged=true。
- [ ] **仍待硬件验收：** 第一波仅验证CPU NV12输入和64²软件兜底，第二波surface实测见后续完成项；Intel/AMD/NVIDIA/混合GPU、真实device-loss与参数拒绝矩阵、CPU/GPU/帧龄恢复测量仍需联合DL2-015验收。进展预算不能中断驱动内部阻塞COM调用。

- [x] **异步缓冲跟进（`fix(DL2-016): pump asynchronous MFT events across buffered inputs`）：** 合法NeedInput→输入1→NeedInput可返回等待下一输入，不再强求每输入100ms内必有输出；先排空旧输出、接收本次输入再交付旧帧，保留各时间戳/IDR元数据，不因INCOMPLETE把不同sample合为一帧。128 credit、16 pending、8MiB输出和100ms无事件/跨调用1秒无输出预算保持有界。
- **验证：** encoder 14项通过（新增6个事件序列/容量/截止回归），lib严格Clippy通过；此为注入事件状态机验证，未代替真实硬件MFT事件执行与恢复矩阵。

- [x] **输出时间戳跟进（`fix(DL2-016): reject unmatched encoder output timestamps`）：** 硬件缺失/未知输出时间戳在既有错误边界重试同一输入为软件IDR并锁定软件；软件未知明确时间戳产生可恢复错误，保留pending元数据，不再假装NeedMoreInput。软件未提供时间戳时保留FIFO契约。
- **验证：** encoder 18项通过，新增当前/旧/未知/缺失时间戳矩阵及合成硬件损坏输出的同帧软件恢复；lib严格Clippy通过。真实厂商时间戳差异继续保留矩阵验收。

- [x] **第二波真实surface编码（`feat(DL2-016): encode owned D3D11 surfaces with tracked sample leases`）：** runtime实际选择CaptureMediaFrame并进入GPU编码；按输入device LUID枚举硬件MFT，只读检查非零D3D11_AWARE，在async unlock后/格式协商前绑定DXGI device manager，包装NV12 subresource为MF DXGI buffer。TrackedSample回调持有独占lease直到最后外部sample引用释放，output/flush不提前归还。沿用原有async credit/pending/output/watchdog预算，真实backend替换先checked预留config后继；CPU/GPU切换、尺寸/设备重绑清旧pending但保持几何epoch。GPU错误从同一owned BGRA明确读回，切软件新config+IDR，当前peer关闭GPU重试；ConfigVersionExhausted仍typed退出、不wrap。
- **本机验证：** NVIDIA720p实际8个surface输入/8输出，强制IDR1/3、709 matrix/limited metadata和CPU NV12/MF显式输入copy=0；6个GPU编码回归全部通过，含真正延迟交付输出的原ID/timestamp/config、故障同帧软件IDR/sticky关闭、版本耗尽，以及已有output后仍持额外sample COM引用→flush/drop encoder与原MFStartup guard→pool仍Busy→最后sample引用释放才精确归还同纹理。不是用“有输出”等价sample已释放。
- **内容/R2联合验证：** GPU AnnexB→强制软件CMSH264DecoderMFT实际解码128帧，Main/High各512²及640×384重配、两phase更新；颜色/方向/UV/严格timestamp与SPS profile77/100、resize新SPS/首IDR通过，左右/上下/UV交换及旧phase反证也通过。真实runtime编码入口→submission归属→HostPipeline→VideoConfig token回归通过：GPU→CPU→GPU与纯codec重配保持epoch，同尺寸原点变化分配新epoch，旧实际GPU输出/旧release不能进入新映射。没有调用真实SendInput或采集桌面。相关库/tests严格Clippy通过。
- [ ] **完整硬件/性能仍待：** Intel/AMD/混合GPU、实际driver/device loss、真实DXGI Acquire/Release、双机/长稳与帧龄/功耗；1080p/1440p/显式4K合成矩阵和可复跑阶段计量由后续015工具切片提供。首批64²软件兜底证据保留为历史，不再代表本机未成功GPU编码；本切片不把CopyResource+VP称零拷贝。

- [x] **代表尺寸内容验证（`test(DL2-016): decode representative GPU frame sizes and display apertures`）：** 在原128帧回归外新增1080p/1440p Main18M及显式4K High40.5M，两phase各4帧共24帧真实GPU编码→软件MFT解码；decoded timestamp/实际SPS profile/首IDR/可见尺寸与tile内容更新均通过、CPU fallback与原GPU读回均0。1080实测coded1920×1088/visible1920×1080，仅依解码器MFVideoArea显示孔径裁padding，检查偶数offset/边界并正确裁Y/UV，不按expected尺寸盲裁。验证模块4项（含裁剪与错轴/UV/旧phase反证）及严格lib/tests Clippy通过。仍不代表复杂桌面视觉质量、真实DXGI或端到端性能通过。
- [x] **当前第二波代码/软件收口（干净源97ff7c0）：** 独立候选实际完成GPU encoder6项、bitstream4项、runtime epoch1项与inventory1项选择运行，连同GPU池8项和example4项共24通过/0忽略；15项原ignored已显式执行，另5项lib默认用例与前套软件范围重叠，不能跨范围相加。保留TrackedSample最后引用释放才归还、同帧回退/配置耗尽拒绝、延迟输出身份与GPU→CPU→GPU/几何epoch边界。完整25软件检查同时通过，代码/软件小项 `[x]`；真实捕获/驱动恢复与同画质性能仍 `[ ]`。见[第二波报告](docs/wave2-gpu-integration.md)。

### DL2-017 · P2 · 将 Windows 音频播放移出高频 UI 节点调度

- [ ] 用 AudioWorklet 或经测量更合适的原生播放路径，替代逐包节点创建。
- [x] `perf(DL2-017): play remote audio through a bounded AudioWorklet`：单个 Worklet 消费 250ms 有界 PCM 环形缓冲，固定 25 包可回收 transfer 池；欠载/过载/过期/抖动指标按低频反馈，声音真正挂起或失败时显示本机重试并避免误静音远端。Bun 1.3.14 `bun test remote-audio` 17 通过、458 断言，实际 Worklet bundle 独立作用域回归及生产构建通过。
- [ ] 真机 UI 长任务下听感连续性、分配率、欠载、设备恢复及端到端音频延迟对照；尚无性能改善百分比或声学测量结论。
- **证据 / 类型：性能结构。** `apps/windows-ui/src/remote-audio.ts:42,115-126` 每包建立 Float32Array、AudioBuffer、AudioBufferSourceNode 并复制；480 样本 / 48kHz 对应约每秒 100 个包时，节点创建也达到同等频率。`14-16` 已有 40ms 提前量和 250ms 调度上限，不能称为无界队列。
- **方案：** 有界环形缓冲、复用内存、抖动/欠载指标和可调目标缓冲；按真实 AudioContext 状态反馈静音、恢复与播放失败。[AudioWorklet](https://developer.mozilla.org/en-US/docs/Web/API/AudioWorklet)可在独立音频线程处理数据。
- **验收：** UI 主线程忙时仍保持音频连续且不过期堆积；比较分配率、长任务、欠载及实测音频延迟，保留明确缓冲上限，不预先承诺改善百分比。

### DL2-018 · P1 · 为 Apple 编码帧输入增加有界解码调度

- [ ] 将逐帧转换/解码提交移出主线程，并约束解码前的积压。
  - [x] 源码实施：FFI callback 复制前限制 header 64 KiB / frame 8 MiB，直接送专用串行解码调度，不再逐帧排 main closure；默认待解码队列最多 6 帧 / 8 MiB / 250 ms，另允许 1 帧原生在途。拥塞/过期清除完整待解码依赖链并等待 IDR，按 generation / stream / config 隔离迟到输出。
  - [x] 输出与转换：UI mailbox 合并最新 pixelBuffer / 配置 / 关键帧请求，主线程再次校验代数和配置；早于 Connected 通知到达的配置、帧和恢复请求各保留最新 1 项并在状态建立后重放，不依赖两种调度机制的 FIFO；Annex B 用原 Data range 扫描，AVCC 一次构建。仍保留 FFI 所有权复制和 CMBlockBuffer 拷贝，未宣称零拷贝或实测性能收益。
  - [x] 自动验证：`python -m unittest discover -s scripts/tests -p test_dl2_apple_media_contract.py -v` 共 9 项静态合同通过（本项 7、043 2）；生产 callback 入口、复制前上限、队列预算、IDR 恢复、串行原生提交和迟到输出保护均入检查；新增 Swift 真实调度器的拥塞/字节/年龄/代数/重配用例及 Annex B 边界用例。
  - [ ] Windows 无 Swift/Xcode，Swift 编译及 XCTest 未执行；仍需 Mac/iOS 阻塞主线程、VideoToolbox/GPU 解码、长稳和 CPU/GPU/内存/分配/copy bytes/解码渲染耗时实测，主项保持未完成。
  - 提交主题：`perf(DL2-018): bound Apple encoded ingress before main actor decoding`。
- **证据 / 类型：性能结构。** `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:569-589` 每个 FFI 事件复制 Data 并 DispatchQueue.main.async，`442-450` 在主线程提交解码；`apps/apple/Sources/DeskLinkAppleCore/H264AnnexB.swift:26-45,80-81` 多次数组/Data 构建，`apps/apple/Sources/DeskLinkAppleCore/H264Decoder.swift:214-220` 再复制到 CMBlockBuffer。
- **现有保护：** `apps/apple/Sources/DeskLinkAppleCore/H264Decoder.swift:17-57` 已合并解码后的输出；本项针对它之前的队列和转换，不能重复报告输出无限缓存。
- **方案：** 专用串行解码队列、按字节/数量/年龄限制输入；保持配置、关键帧和 delta 依赖，必要时请求关键帧；NAL 用 range/slice 一次构建，UI 只发布最新 pixelBuffer 和低频指标。
- **验收：** 主线程阻塞后输入队列和 frame age 有上限；记录分配/copy bytes、解码/渲染时间、CPU/GPU/内存；不能任意丢 H.264 delta 破坏参考帧链。

### DL2-019 · P1 · 中继媒体拥塞不能阻塞连接控制与音频

- [ ] 分离读取与发送调度，为各类数据设置独立预算和时效。
- [x] 调度隔离切片：每连接固定 3 个媒体 sender，音频/光标/视频独立有界 FIFO（32/8/256 包；60/40/250ms 总年龄），入队不等待；去掉逐包 task，关闭时取消 sender；入队绑定目标代数并在发送前复核，独占 `Bytes` 可原地改路由头。
- [x] 自动回归：媒体队列 3 项测试；真实 loopback QUIC 通过 UDP ACK 黑洞触发 `send_datagram_wait` 超时后，验证音频仍可入队、另一会话输入/控制正常、新可靠流仍被接受及全部 sender/容量回收。协议 13 整合后 relay 合计 34 项常规测试（5 lib + 1 bin + 28 session）及 Clippy `--all-targets --locked -- -D warnings`、rustfmt 通过。
- [x] 可重复本机测量切片：1 条已确认 ACK 黑洞拥塞会话 + 4 条健康会话并发，各取得 300 个合成输入往返/音频 event 到达样本；协议 13 基线上 p95/p99 分别为 0.9534/1.5978ms 与 0.5286/0.6031ms，30 个媒体 sender 最终全部回收。debug loopback、非真实注入/播放/双机容量；测量用例默认 ignored，本轮已显式执行通过。提交主题：`test(DL2-019): measure local scheduling under media backpressure`，完整环境、命令与结果见调度记录。
- [ ] 帧/依赖级丢弃及真实多会话性能验收：中继当前只见密文，本切片仅实施逐通道包预算，仍共享 QUIC 拥塞窗口/发送缓存；输入/音频 p95/p99、CPU/吞吐、容量及长稳未测。实现和验证边界见 [中继调度记录](docs/phase-two-relay-scheduling.md)。提交主题：`fix(DL2-019): isolate relay media queues from connection reads`。
- **证据 / 类型：性能结构。** `server/relay/src/lib.rs:92,1112-1135` 共用 256 个 datagram 转发槽，耗尽时在连接 select 分支内 await permit，暂停同循环 accept_bi/closed；`1392-1415` 视频、音频、光标共用路径，每包建任务/Vec，单包等待上限 250ms。
- **现有保护：** `server/relay/src/lib.rs:1080-1109` 已把建立的可靠通道放入独立 forward_reliable 任务，它们仍能继续转发；直接受阻的是新可靠流的接受、同循环关闭处理和后续 datagram 读取，不能据此断言所有现有键鼠/控制都被槽位直接阻塞。
- **方案：** 按连接常驻 sender 与有界队列，按音频、光标、视频的时效调度；视频以帧/依赖为单位丢弃过期数据；可靠控制/输入有独立预算，单个慢接收端不占满其他会话资源。
- **验收：** 慢接收端、关键帧突发、多会话压测下记录输入/音频 p95/p99、任务数、队列年龄、CPU/吞吐；关闭/连接事件不被媒体槽位等待卡住。实际容量需测试。

### DL2-020 · P2 · 关键帧可靠批次一次计长、一次编码

- [x] 移除组批时反复克隆并序列化整个候选批次。
- **证据 / 类型：性能结构。** `apps/windows/src/runtime.rs:2263-2293` 每增加分片先 clone 当前批次/datagram，再 encode 判断大小；`2302` 发送时再次编码。`crates/desklink-protocol/src/lib.rs:66` 批次上限 60 KiB。
- **方案：** 依据 wire header 与分片长度增量计算边界，最终一次编码；复用 Bytes/切片，保留协议长度验证。
- **验收：** 最大合法关键帧的分配/复制次数下降；精确边界、跨批、超限和 round-trip 不变。此重复工作有单批大小上限，不是整个会话无限增长。

- [x] **已实现并验证（`perf(DL2-020): build reliable video batches once in reusable buffers`）：** 协议builder一次预留60KiB，按3字节header+2字节片长增量校验/复制，Host发送直接复用最终wire切片，clear保留同一allocation；旧wire及编码入口保持。
- **验证：** protocol `--lib --tests --locked` 34既有+4新增通过、1基准忽略；Windows `--lib direct_video_tests --locked` 2通过；两crate lib Clippy严格通过。覆盖精确60KiB、4096片、跨批无丢失/重复、超限不修改、round-trip及固定buffer指针。
- **可复跑构造基准：** `python scripts/run-windows-cargo.py test -p desklink-protocol --release --test video_batch_builder benchmark_maximal_keyframe_batch_construction --locked -- --ignored --nocapture`：4096片/71批/4276309 wire字节，两实现输出相同；单次本地CPU样本旧候选克隆编码8946μs、builder1665μs。此结果仅说明该构造工作负载，不外推网络/GPU/端到端收益。

### DL2-021 · P1 · 慢磁盘写入与远控 async 调度隔离

- [ ] 为文件读写提供带字节预算、取消和完成语义的专用 worker。
- [x] 控制端接收切片 `perf(DL2-021): isolate controller file receive completion from media ingress`：复用媒体组 `c2ec149`（本分支来源提交 `eb6616d`）的 IncomingFileWorker；每接收文件单个有界 coordinator，2×48KiB 排队和当前块，主循环只提交命令并独立消费磁盘完成。队列满时 next_event_with_transfer(false) 仅暂停文件 lane，继续消费视频/音频/控制和本地取消；写入 ACK 后进度、hash/flush/rename 后完成。创建/写入/完成立即取消与迟到成功共 6 项受控慢盘测试通过；transport 24 单元+17 localhost、FFI 12 单元+5 controller_runtime 集成通过；管理库完整 113 通过、1 签名包忽略（后续新增两个取消边界聚焦通过）。
- [ ] 真机慢盘/空间耗尽/杀毒扫描与性能验收；控制端出站 pre-offer metadata、队列保护存储仍有同步路径。远端取消受同一可靠文件 lane 回压约束，不声称可越过已满通道；本地取消不等待磁盘 gate。
- [x] 独占收尾补测 `test(DL2-021): preserve exclusive receive ownership across controller retries`：复用媒体 `6900d1e`（本分支 `fb60e5f`）的实际 staging 许可，补测 Busy 原样拒绝、旧 worker preserve/cancel 未结束时重试 Busy、清理完成后新 ticket 独占成功且内容不被旧清理删除。控制端 pipeline 9 通过；本分支 Windows transfer::tests 19 通过，含恢复存储的 transfer 过滤 26 通过。编排回归用受控 lease，物理 Windows 写独占由实际 transfer 单测验证。
- **证据 / 类型：性能结构。** `apps/windows/src/runtime.rs:3035-3044` 直接调用 `apps/windows/src/transfer.rs:360-373` 的同步 write_all 与 hash。初始化及完成已有 spawn_blocking，不能写成全部 I/O 都未隔离。
- **方案：** 长驻磁盘 worker 或有界异步管线，网络回压跟随落盘速度；不要每个 48 KiB chunk 新建 blocking task。两个传输方向一起核对；校验/flush 成功后才发布完成。
- **验收：** 慢盘、云盘、杀毒扫描、空间耗尽、取消同时发生时，媒体/输入仍满足预算、内存有界、残留可清理；不能显示尚未落盘的虚假完成。

- [x] **已实现并自动验证（`fix(DL2-021): isolate host receive writes on bounded disk workers`）：** `IncomingFileWorker` 每文件长驻磁盘线程，进程最多8线程，队列2×48KiB（另有当前块）；写入/hash/sync/rename及Drop保留均离开async线程。Host接入create/write/finish/preserve/cancel，校验并sync成功后才回完成；原出站有界blocking生产者保持。
- **验证：** `python scripts/run-windows-cargo.py test -p desklink-windows --lib transfer::tests --locked` 16通过；后续静态检查收口取消分支（`style(DL2-021): simplify incoming transfer cancellation`）；覆盖人工阻塞磁盘时current-thread Tokio定时器仍执行、队列预算、取消清理、保留续传、错hash、完成后发布。Windows删除前先关闭staging文件句柄。
- [ ] **跨组及实机未完成：** Windows控制端接入由产品组负责；慢盘/云盘/杀毒/空间耗尽时媒体与输入预算需双机测量。取消不能中断已进入OS的磁盘syscall，线程总量有界，不宣称立即物理清理。

- [x] **暂存所有权跟进（`fix(DL2-021): hold exclusive staging ownership through disk cleanup`）：** 异步Drop/保留期间，以规范化暂存路径RAII许可持有至文件关闭和清理结束；同文件重连先返回Busy，避免旧新worker并写/旧取消删除新文件。Windows写句柄只共享读取，续传先独占打开同一handle再扫描前缀；丢弃/陈旧清理同样遵守许可。
- **验证：** transfer单测23通过、lib严格Clippy通过；3个新增回归覆盖旧写阻塞→Drop→别名路径重连Busy→释放后精确续传、旧cancel不删新文件、Windows绕过API的写/删除也被拒绝。未把不能中断的OS写线程视为已经退出。

- [x] **初始化取消窗口（`fix(DL2-021): preserve resumed files when receiver creation is cancelled`）：** create future在opened.await期间被断连取消，磁盘线程后续ready发送失败时，先走同一preserve/sync路径再退出，避免直接Drop误删已校验旧prefix；文件/写许可/线程额度仍在磁盘线程释放。保留失败不发布完成，也不宣称成功保留。
- **验证：** transfer 28通过、lib/tests严格Clippy及定向rustfmt/diff检查通过。真实spawn_with→create路径在已取得独占句柄、读取/哈希前阻塞，poll Pending后丢创建future，放行并等实际线程退出；9次取消后原4字节prefix/offset/hash/元数据不变，再续传通过完整哈希。另覆盖新空part保留、显式cancel删除、实际初始化TooLarge错误不删旧prefix、Windows只读handle导致sync_data返回IoFailed且未发布。资源型worker测试共享异步测试锁，未提高生产8线程上限或把外部忙碌当作泄漏。

- **联合复跑：** `65c70fd` 的实际初始化取消保留回归、worker测试限流与新鲜路径清理同入最终Windows283项通过计数；未删改该来源测试或退回旧worker版本。最终证据见 `docs/windows-joint-integration.md`。

### DL2-022 · P2 · 从 GDI 兜底恢复到高性能捕获后端

- [ ] 增加有退避、有限失败预算的 DXGI 恢复探测。
- **证据 / 类型：能力缺口、性能结构。** `apps/windows/src/capture.rs:551-565` 在运行时 DXGI 故障后永久替换成 GDI；`483-485` GDI 等待后完整截图，本会话没有回到 DXGI 的路径。
- **方案：** 保留现有可靠兜底，后台探测设备/桌面恢复，再按代数切回；显示实际后端和降级原因，防止来回振荡。
- **验收：** 睡眠、RDP/桌面切换、显示器恢复后，DXGI 可在不结束会话的情况下恢复；比较恢复前后资源与帧龄。不得为追求 DXGI 反复打断稳定 GDI。

- [x] **已实现并自动验证（`fix(DL2-022): retry DXGI recovery while retaining GDI fallback`）：** GDI期间1秒后探测真实DXGI帧，失败按2/4/8/16/32秒退避、6次后60秒冷却；恢复后30秒内再次失败进入60秒迟滞。构造成功不足以切换，探测失败保留GDI；几何变化留给拓扑协调而不暗改输入坐标。
- **验证：** `python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked capture::` 7通过，1个桌面依赖项忽略；3个新增用例覆盖预算、冷却与稳定窗口。
- [ ] **仍待真机/诊断接通：** 睡眠/RDP/桌面切换和显示器恢复矩阵、后端/降级原因界面及资源/帧龄对照；连接内拓扑变化依赖DL2-013剩余工作。

### DL2-023 · P2 · 分离清晰度、帧率、码率与内容模式

- [ ] 重做能力协商及自适应策略，在能力允许时支持 4K/60fps 等配置。
- **证据 / 类型：能力限制。** `apps/windows/src/runtime.rs:106-123` 主要为 720p30/1.5Mbps、1080p30/2.5Mbps 和 Sharp；`apps/windows/src/encoder.rs:4-5,14,25-47` 常规 Sharp 上限 1440p30/18Mbps，4K 仍为显式实验；`apps/windows/src/runtime.rs:163-212` 主要依据丢包、decode queue 和 freshness 调档。
- **用户影响：** 档位跳转同时改变多个维度，文字清晰度与运动流畅度无法分别选择；高分屏/高刷能力未成为完整产品配置。
- **方案：** 连续码率与受约束的分辨率/帧率协商，加入编码耗时、排队年龄、吞吐和渲染压力；静态文字、滚动、视频模式分别决策并有迟滞。先完成 DL2-014～016。
- **验收：** 带宽阶跃、丢包、弱 CPU/GPU、静态文字及视频播放对照清晰度、帧龄、切档次数和恢复速度；不能把 4K60 对所有设备一刀切开启。

- [x] **原生码率API切片（`feat(DL2-023): add transactional native bitrate updates`）：** `H264Encoder::try_set_bitrate` 在能力可修改/可回读时事务写Mean/Peak并验证，成功不重建reference/config/dims/fps；拒绝只有确认恢复旧值才返回false，不确定恢复返回错误要求调用方重建旧设置。见 `docs/windows-encoder-backends.md`。
- **验证：** encoder 8项通过，含4事务故障/回滚、软件合成编码前后配置/参考连续性；lib严格Clippy通过。当前软件MFT返回“不支持动态修改”，未声称真实动态成功或硬件收益。
- [ ] **完整策略/硬件仍待：** 独立帧率/内容模式、扩充能力协商及4K/60门槛；带宽/弱硬件/清晰度/帧龄/切档与恢复矩阵尚待。

- [x] **生产策略接入（`feat(DL2-023): adapt bitrate independently within video presets`）：** Automatic下档内20%/严重35%降码率，原生确认后8反馈窗口冷却、12健康窗口后5%恢复；保持分辨率/帧率，手动偏好恢复档位ceiling。实际值随质量/Profile应答回传，拓扑/Profile保留码率；跨配置失效旧目标且阻止新rate观察，避免旧档位rate晚到。队列扩为4+1有界。配置future等真实原生完成、随peer取消，不把5秒超时当已拒绝；不支持/下限/渲染压力沿既有档位回退。
- **验证：** Windows lib 159通过/2忽略、独立policy 8通过、配置队列6通过，严格lib+tests Clippy通过；含5.1秒迟到原生应答仍正确确认、手动门控、跨配置旧请求失效、精确上下限及回滚。说明/复跑见 `docs/windows-adaptive-bitrate.md`。当前软件MFT不支持动态码率，未宣称硬件成功或网络性能收益。

### DL2-024 · P2 · 将低延迟直连从视频优化扩展为完整数据路径

- [ ] 设计认证后的多通道 LAN 数据面，再按测量决定公网 NAT 穿透方案。
- [x] 第二波来源整合切片：在独立 `codex/dl2-lan-runtime` 合并冻结 Windows joint `5782341` 与完整网络 `4b15561`，保留双亲历史；七处冲突逐块确认，保留真实 epoch/owned input/媒体与任务生命周期。共享四包 215 通过、4 ignored；Windows 283 通过、17 ignored。只证明整合，LAN 生产接线和 Apple 完整上下文仍待后续；接口计划与来源边界见 [生产 LAN 接线记录](docs/lan-runtime-integration.md)。主题 `merge(DL2-024): combine frozen Windows and network snapshots`。
- [x] 真实 relay backend 接口切片：QuicClient 为八条业务 lane 提供 generation 绑定的原密文发送/读取，以及不消费事件的连接健康观察；旧 generation 与零 generation 拒绝，控制 lane 保留替换 peer 的握手保护。新增真实 loopback 八 lane 字节一致与健康观察回归，transport 45 单元+23 localhost 通过、严格 Clippy/fmt 通过；尚未启用生产 LAN。主题 `feat(DL2-024): expose generation-bound relay business lanes`。
- [x] 完整 Apple 稳定来源整合切片：双父合入 `f954a68` 完整历史，保留捕获 context/scoped producer/owned input/1 秒 Control 与 Windows Transfer 旁路；修正窗口构造冲突，以只降不升的有类型 flow-control 入口同时保留 052 资源上限与真实 2KiB 背压夹具，新增三种超限拒绝。六包 592 通过/21 ignored（FFI88），严格 Clippy/fmt 通过；局部补已安装 MSVC PATH/INCLUDE 后实际 C ABI 布局通过，未跳过。尚未接通生产 LAN/真实 Mac，证据见接线记录。主题 `merge(DL2-024): integrate complete Apple input lifecycle source`。
- [x] 生产 transport 适配切片：复用唯一协议14 LAN合同，新增真实 QuicRelayBackend/SessionDataPlane、私有协商/attach authority、独立有界 lane、五条 owned relay writer、逐 lane 远端 admission fence 与原密文回退/去重；屏障严格绑定 committed mask/switch generation，不从 query open 新序号。transport 49+23、十项真实 relay 适配回归通过，17 秒静默 LAN 单独显式通过，取消/crypto 锁/替换 peer/close_and_wait 任务归零；真实 7B send+48B stream credit 的 fence 写背压验证不增加300ms预算。强制 teardown 部分帧仍可触发原 framing guard 关闭物理relay，未冒称完全无损。细节见 [会话 transport 记录](docs/lan-session-transport.md)。主题 `feat(DL2-024): add negotiated session business transport`。
- [x] 传输层独立切片：新增 opt-in DLD1/DLB1 数据面，复用既有认证 probe 与 Noise 会话，提供八通道有界队列、可靠流优先级、逐通道回退、同密文去重、截止时间与取消；收讫使用 AEAD 内随机 128bit token，拒绝伪造/旧 ACK，不推进反向业务 replay window。提交主题：`feat(DL2-024): add authenticated multi-lane LAN transport slice`。
- [x] 本地自动验证：固定 Rust 工具链通过 transport **35 项单元测试（其中新增 LAN 数据面 11 项）+16 项 localhost 集成测试**，Clippy `--all-targets --locked -- -D warnings`、包级 fmt 与 `git diff --check` 通过。覆盖 relay backend 离线时八通道双向传输、慢文件消费者与输入/音频隔离、LAN/relay 去重、伪收讫、错会话/错 lane、双向 >128 包及持锁取消；不涉及公网或真实输入注入。
- [x] 共享探测接入复核：复用 DL2-011 的 `3277c2a` 与 `0836a8b` 独立提交，保留 LAN 八个业务通道，探测统一使用独立 `DirectProbe=8`；crypto 36、transport 38+16、relay 34、FFI 39、Windows DirectLan 2 项测试通过，相关 Clippy/fmt 通过。仅透传输入业务正文，DL2-013 的 `stream_id/input_epoch` 编解码及校验等待 Apple/FFI 单一契约，不另造格式；主任务仍未完成。提交主题：`docs(DL2-024): verify shared probe integration boundaries`。
- [x] DL2-013 合同传输回归：复用 `bc6c0a6`，Host 夹具先通过正式 VideoConfig codec 公布非零上下文，Controller 从收到配置取上下文再用 `encode_input`；LAN 直传、缺收讫后的原密文 relay 重试及去重均保留 `stream_id/input_epoch`、u64 边界与开头零字节，仅用 `decode_session_input` 解码。本分支 protocol 47、transport 43+16、relay 34 项通过，protocol/transport Clippy 与 fmt 通过。该合同是生产零值占位会被拒绝的不可交付过渡，不代表 Host epoch 校验或业务链路通过。提交主题：`test(DL2-024): preserve protocol input contexts across LAN fallback`。
- [x] Windows/实际 ControllerRuntime 与 owned FFI worker 生产接线切片：唯一14真实双端能力交集冻结正文模式，Host/Controller 八 lane 统一收发与解密一次、原 context 不重贴、原1秒Control/owned input 保留；有界 probe 与候选过期只降级，已开始输入取消关闭原会话并清键，Transfer局部失效取消worker/暂停队列而不截 Input/Control。Mac/iOS声明None且未实现消息明确拒绝，不借opt-in伪称支持。正式Host Control loop持续pin probe，协议/版本生成及现有GPU/文件语义不变。主题 `feat(DL2-024): connect Windows runtime business lanes to authenticated LAN`。
- [x] 第二波真实本地闭环：Host/Controller正式握手与切换、断fixture relay后既有LAN、双向344087B真实磁盘/hash/完成ACK、满/慢Transfer时Input+真实Opus独立、取消owned Input严格Down→Up及克隆拒绝；七包762常规通过/23 ignored、Python207、严格Clippy/fmt及C ABI生成检查通过，17秒静默专项另实际通过。fixture不注入OS/不读写真实Downloads，120ms仅合成回归预算；强制部分帧teardown仍可能关闭物理relay，LAN-only结束未新增业务ACK而保留unconfirmed。来源、完整口径与日志见 [生产接线记录](docs/lan-runtime-integration.md)。
- [ ] 平台/真实环境验收：LAN发现与首次离线本地配对、Mac/iOS原生LAN能力、双机捕获/呈现/注入/音频、长稳/吞吐/分位延迟、公网NAT/UDP限制与回退实测仍待实施/验收；现有授权LAN延续已本地验证，不能外推首次离线连接或真实容量。主024继续保留未完成。
- **第一波基线证据 / 类型：历史架构能力限制。** 原快照 `video_egress.rs:5-8` 只优化视频，`runtime.rs:2424-2428` 音频走 relay，`quic.rs:925-930` 为可靠通道；这些行号仅对应原快照。第二波已接通上述真实能力协商与业务路径，剩余边界是未启用平台、发现/配对和真实设备/网络验收，不能用第一波限制否定已实现代码，也不把本地回环外推实机效果。
- **方案：** 同一已认证路径承载独立优先级的输入、控制、音频和文件，并支持逐通道切换、回退和去重；完善局域网发现/本地配对与 relay 不可达时的行为。公网打洞作为独立实验，不把“换 WebRTC”本身当性能结论。
- **验收：** 同网输入延迟符合实际 LAN 路径；断开公网后按定义保持本地连接；切换不重放按键、不串会话、不中断授权。公网方案另测不同 NAT/IPv6/UDP 限制与回退时间。

### DL2-049 · P3 · QuicClient 按 relay 地址族绑定本地 socket

- [x] `QuicClient::connect` 对 IPv4 relay 绑定 `0.0.0.0:0`，对 IPv6 relay 绑定 `[::]:0`；绑定失败仍通过带具体地址的 InvalidConfig 返回，不改协议/输入格式。修复前本地 `::1` 实证失败 `invalid remote address`，修复后真实 RelayServer 的 v4/v6 连接、join、目录发布/查询、可靠 VideoConfig/Input 与视频/音频 datagram 回环及容量回收均通过。
- **验证：** 2 项地址族回归及 transport 43 单元 +16 localhost 测试通过，相关 Clippy、rustfmt、diff 检查通过；仅 loopback 合成传输，未声称公网 IPv6 路由、NAT、双栈 socket 混用或真实输入注入。主题 `fix(DL2-049): bind relay clients to the matching address family`。

### DL2-050 · P3 · FrameAssembler 重复分片先覆盖后判重

- [x] 重复分片保留首次内容并丢弃后续。
- **证据 / 类型：源码缺陷。** 审查基线 `crates/desklink-video/src/packet.rs:182-188` 的chunks.insert先替换旧值再返回DuplicateChunk，内容不一致重传可污染组帧；CodeGraph定位后以本分支实际源码确认。
- **修复（`fix(DL2-050): preserve first payload when video chunks repeat`）：** BTreeMap entry已占用时拒绝新payload并饱和增加dropped_chunks，未占用才插入；重复不延长原分片期限。
- **验证：** `python scripts/run-windows-cargo.py test -p desklink-video --lib --tests --locked` 6单元+17排序=23通过；同package严格Clippy通过。新增不同payload首内容保留/相同内容重复/1000包洪泛三项，覆盖乱序补片、计数取走复位、单partial与120ms原期限。该条不依赖真实硬件验收。

## 5. 功能完备与用户流程

### DL2-025 · P1 · 统一 Rust、C 头文件和 Swift 的能力契约

- [ ] 补全 ABI 命令/事件，并自动检查两份 C 头文件与实现一致。
- **证据 / 类型：能力缺口、契约缺陷。** `crates/desklink-ffi/src/controller.rs:330-379` 已有显示器、音频、画质、transfer 方法；`crates/desklink-ffi/src/worker.rs:70-75` 只有 Input/ReleaseAll/Keyframe/Shutdown；`crates/desklink-ffi/include/desklink.h:46-57` 事件止于 ReleaseAll，而 `crates/desklink-ffi/src/lib.rs:115-116` 已有 Transfer=11、Audio=12。Apple 还维护 `apps/apple/Sources/DeskLinkC/include/desklink.h`。
- **方案：** 为所有命令、事件、结果、错误及 capability 建立唯一契约来源，生成/校验 C 与 Swift 声明；事件携带代数、版本、可恢复性。通过有界 worker 命令暴露现有 runtime 能力。
- **验收：** 每个公开枚举、struct 布局、长度/所有权与命令均有 ABI 测试；新增事件不能无声落入 Swift default。此项是 DL2-026、027 和多个输入修复的共同前置。

- [x] ABI 基础切片：Rust 公开枚举/struct/extern 签名生成两份字节一致 C 头；新增有界显示器/音频/画质/profile 命令及 transfer 消息入口，统一取消截止；认证能力事件含远端平台/尺寸/profile，Swift 显式接收 Control/Transfer/Audio/Capabilities。Rust 16 单元 + 2 ABI 布局回归通过（MSVC 实际编译所有公开 struct 的 sizeof/align/offset，与 Rust 对照）；Python 4 合同、生成检查通过。主题 `feat(DL2-025): generate ABI headers and expose bounded session commands`。
- [ ] 其余 typed Swift 事件/能力消费及每种命令端到端回执；当前最新消息暂存不等于音频播放或文件工作流，事件所有权由生成头明确为仅回调期间借用。Swift/Xcode 目标布局与端到端调用尚待 Apple 环境。

- [x] 契约同步补充：复用产品组协议 13 独立提交 `9979c6424cf99f704feb212c7700833fce466dae`（本分支 cherry-pick `da44ff2`），生成 C/Swift 版本常量并对齐光标 fixture；修正生成 C uint32_t 的 Swift 参数类型、显式测试 C 模块依赖；已知 Host Transfer 不再落入终态错误。Python FFI 6 + 光标 6 合同、Rust 光标 fixture/InputState/版本边界 5 项通过。主题 `fix(DL2-025): align generated Swift and protocol 13 contracts`。

- [x] FFI 直连发送与入站探测改用已提交共享锁接口，网络 await 不再持加密锁。复用媒体组 `3277c2a41af0ac923e1daa1c5cab5d952b355b9b`，本分支依赖提交 `3b4923f`（排除与基线无关的 DL2-010 纯格式冲突片段）。Rust FFI 全套 48、transport shared 探测 2 项通过，FFI all-targets Clippy `-D warnings` 通过；Python 全部 155 项通过。主题 `fix(DL2-025): reuse cancellable shared-crypto direct probes`。

### DL2-026 · P1 · 补齐 macOS → Windows 的会话功能闭环

- [ ] 将已有底层功能真正接到 AppleCore 与 macOS 会话界面。
- [x] 首个代码切片：严格、有界的 Control 状态解析接入显示器 / 当前屏、画质偏好 / 实际 preset、音频可用状态及可恢复输入受限；Mac 会话菜单通过真实有界 FFI 命令发送切屏和画质请求，仅在 capabilities + connected 时启用。排队成功只显示等待，远端回执才确认；不匹配 / 5 秒无回执可重试，输入恢复后清除受限提示。主题 `feat(DL2-026): connect Mac display and quality controls to remote acknowledgements`。
- [x] Windows 静态与 wire oracle：`python -m unittest scripts.tests.test_dl2_mac_session_contract -v` 6 项通过；新增 Swift 解码、回执 / 拒绝 / 超时状态回归及跨语言 Rust postcard fixture，Swift/AppKit 尚未运行，Rust fixture 由本组统一验证后记录。
- [ ] 其余闭环与真机：适应 / 1:1 / 会话全屏的统一视频与输入几何、音频实际播放 / 静音 / 音量、双向剪贴板、双向文件流程及所有 Mac → Windows 交互 / 恢复仍待完成；typed 音频状态不是音频播放，菜单源码和合成回执不是双机结果。
- **证据 / 类型：能力缺口。** `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:336-350` 只消费 state/error/video/cursor，Control/Audio/Transfer 落入 default；`apps/macos/Sources/DeskLinkApp/Views/SessionView.swift:56-84` 缺少相应会话操作。Rust 有方法不能代表 Mac 用户已可使用。
- **分项交付：** 多屏列表/切换；会话内适应/1:1/全屏操作与清晰度；音频播放/静音/音量及失败恢复；纯文本剪贴板双向操作；文件双向选择、确认、进度、取消、重试与断点恢复。macOS 原生窗口全屏已有系统入口，本项不把它说成完全无法全屏。
- **方案：** capability 驱动操作可用性；每项提供等待、成功、拒绝、失败和下一步，不支持时准确说明；布局沿用平台原生习惯，控件命名与 Windows 保持任务语义一致。
- **验收：** Mac → Windows 每个分项都完成真实用户动作和错误恢复；接收文件仍经被控端授权，换设备不能恢复到错误目标；与 DL2-025、029～032 联合验证。

- [x] Bridge 实际消费 DisplayList/VideoQualityState/AudioState/InputState，重复同值回执仍推进 revision；输入受限与恢复独立于会话终态，新能力/断开清空。Rust 真实 codec→Swift 六组 fixture 回归 1 项通过；Python 会话合同 6 项通过。主题 `feat(DL2-026): connect Mac display and quality controls to remote acknowledgements`。

### DL2-027 · P1 · 在 Apple 会话呈现远端独立光标

- [ ] 解码并绘制形状、热点、位置和可见性，不依赖视频中烘焙光标。
- [x] 位置切片：严格解码真实 postcard CursorUpdate，按 stream/sequence 隔离迟到与乱序；Mac/iOS 在实际视频几何（含 iOS zoom/pan）上独立绘制中性位置圆环并响应隐藏。连接恢复/关闭隔离旧位置，视频冻结或关键帧恢复期间仍消费当前会话位置。Metal 仅帧身份、几何或布局尺寸变化时请求重绘。`python -m unittest discover -s scripts/tests -p test_dl2_apple_cursor_contract.py -v`：6 项通过；`python scripts/run-windows-cargo.py test -p desklink-protocol --test apple_cursor_contract --locked`：1 项通过，生产 Rust codec 校验 Swift 的可见/隐藏/边界字节 fixture。提交主题：`fix(DL2-027): render isolated Apple cursor position updates`。
- [ ] 完整形状/hotspot/cache 尚缺上游数据：协议 v12 仅发送 opaque shape_id，Windows 当前值来自 hCursor，未传 BGRA、尺寸或热点，不把该句柄推断为远端形状。需扩展上游生产契约；本切片圆环只表达位置。Swift/Xcode、真实 I-beam/resize/忙碌/隐藏及不同 DPI/缩放四角验收仍待运行，尚无性能实测。
- **证据 / 类型：能力缺口。** `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:346-348` 已写 cursorOverlay；`apps/apple/Sources/DeskLinkAppleCore/DeskLinkEvents.swift:202-209` 保存 encodedUpdate；macOS `apps/macos/Sources/DeskLinkApp/Views/SessionView.swift:18-22` 只有视频和输入层，Apple 展示路径没有消费此 overlay。
- **方案：** 独立低延迟光标层、shape cache、统一视频几何与 DPI 映射；处理隐藏、切屏和缩放后的热点。
- **验收：** 文本 I-beam、resize、忙碌、隐藏光标与四角热点在不同 DPI/缩放模式正确；仅光标变化不强制整帧重绘。

### DL2-028 · P1 · 重做 Apple 保存设备模型与长期重连

- [ ] 将设备身份、用户名称、授权类型和可更新的连接材料分离。
- **证据 / 类型：能力缺口。** `apps/apple/Sources/DeskLinkAppleCore/SavedHostStore.swift:4-9` 仅保存 UUID/serverName/sessionID 等；`apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift:96-103` 用同一 relay serverName 作为多个设备的主标签。`apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift:210-218,467-477` 回放旧材料；`crates/desklink-ffi/src/lib.rs:881-889` 已保存材料的 secure 重连分支把 expiry/directory 置空。当前 directory 连接已有重新 lookup 与过期处理（`crates/desklink-ffi/src/worker.rs:694-734`），缺口是保存模型与后续回放未保留这些语义。
- **用户影响：** 多台电脑难区分；保存到 Keychain 不等于主机重启和 session 重建后仍可长期重连。
- **方案：** 稳定设备 ID、公钥、可编辑别名、授权类型/过期、上次连接、连接材料分别建模；固定访问重新解析目录并核对身份；临时授权失效明确要求重新连接。
- **验收：** 同一 relay 的多台电脑清楚可辨；固定主机重启/session 重建可恢复，临时密码刷新/过期与撤销不得盲目循环；设备身份改变需重新确认。

- [x] 保存/重连代码切片：FFI 与 Keychain 保留明确授权类型、expiry、目录 ID/访问码；目录重查必须匹配已批准公钥，认证成功才发布材料。临时过期与未知旧记录不升级永久；明确持久授权退避预算耗尽后仍可取消地等待。列表支持身份名称/本机别名，刷新保留 UUID/别名并更新材料。Rust `desklink-ffi --lib --tests --locked` 47 项通过（含 C/Rust 实际布局、身份 pin、授权边界与持久等待取消）；Python SavedHost 合同 7 项通过。主题 `fix(DL2-028): persist authorization and pin saved host reconnects`。
- [ ] Mac Keychain 与真实固定主机重启/session 重建、临时刷新/撤销及身份变化确认验收；Swift 测试已补但本 Windows 环境未执行。

### DL2-029 · P1 · macOS 连接和等待批准期间提供取消

- [ ] 统一连接进度页，所有非终态等待均可取消或更换目标。
- [x] 代码与静态合同：Mac 连接表单的连接/审批等待区提供直接取消和 Escape，保留设备 ID/密码；会话恢复期提供直接取消。iOS 等待区明确取消动作并阻止重复提交/扫描。取消复用 generation 隔离的 `disconnect()`。`python -m unittest discover -s scripts/tests -p test_dl2_apple_cancel_contract.py -v`：5 项通过；`git diff --check` 通过。提交主题：`fix(DL2-029): expose cancellation throughout Apple connection waits`。
- [ ] Mac/iOS 编译与真实逐阶段取消、取消后更换目标、迟到批准/错误/视频不复活验收；本 Windows 环境无法运行 SwiftUI/Xcode。DL2-004 的异步停止与 ≤2 秒主线程响应实测仍是依赖，不以静态合同代替。
- **证据 / 类型：能力缺口。** `apps/macos/Sources/DeskLinkApp/Views/ConnectView.swift:63-71` 及 `apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift:108-119` busy 时只禁用连接；`apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift:118-121` connecting/pairing 尚未进入有断开按钮的会话页；`crates/desklink-ffi/src/controller.rs:44-47` 握手可等待 150 秒，等待批准/能力协商可等待 120 秒。
- **方案：** 展示查找、建立连接、等待批准、恢复等状态，始终提供取消；保留可重试的用户输入；generation 隔离迟到批准、错误及视频。
- **验收：** 每个阶段立即取消，再连另一台设备，旧结果不复活；取消本身满足 DL2-004 的时间和主线程响应预算。

### DL2-030 · P2 · 支持大文件与目录的完整传输体验

- [ ] 升级单文件边界与目录清单，完善大数据量续传、校验和进度。
- **证据 / 类型：能力限制、性能结构。** `crates/desklink-protocol/src/lib.rs:40-42` 单文件上限 **256 MiB**，`apps/windows/src/transfer.rs:35-41` 发送准备即拒绝；当前以单文件 offer/chunk 表达。`apps/windows/src/transfer.rs:90-119,265-307` 和 `apps/windows/src/runtime.rs:3172-3198` 恢复会重复扫描已有前缀进行 hash。
- **方案：** 保留协议已有的 u64 尺寸/偏移（`crates/desklink-protocol/src/lib.rs:463,473,481`），贯通大文件上限、UI/FFI 计数与空间预估；增加文件树 manifest、分块校验和可复用断点证明。重名冲突、符号链接/路径边界、部分成功、失败项重试与取消组成同一工作流，不能仅调大上限，把慢扫描转移给用户等待。
- **验收：** >4 GiB 文件、深层目录、中文/长名称、同名、稀疏文件和慢盘；断网/重启后仅在身份及源内容匹配时恢复；完整校验后才显示完成。保留现有双向传输和确认机制，不能误当它们未实现。

- [x] **已实现并自动验证（`perf(DL2-030): verify resumed sources with one streaming hash`）：** 新 `OutgoingFileReader` 在同一已打开句柄/hasher上只扫描一次已收前缀，再从精确u64 offset发送后缀及完整hash；Host出站接入，保留mpsc(2)背压。首尾验证size/mtime，Windows只读共享禁止发送期间写入/删除源文件；不改变既有确认和身份约束。
- **验证：** `python scripts/run-windows-cargo.py test -p desklink-windows --lib transfer::tests --locked` 20通过；新增中块续传/最终hash、源变化先拒绝、取消/未读完不允许完成、Windows源写入删除排斥4项。此切片消除单次重试的重复扫描，仍有每次重试O(prefix)校验。
- [ ] **仍待跨平台/真机：** macOS 文件落盘/确认适配、真实双机 >4GiB/目录、真实 OS 重启与慢盘硬件、极限规模目录验收。Windows 本地实现及自动夹具证据见后续子项，不替代这些设备证据。

- [x] **会话退出切片（`fix(DL2-030): cancel file producers when host peers disconnect`）：** 出站任务由HostOutgoingTransfer RAII持有，peer退出/错误/取消均置取消并abort异步sender，释放receiver唤醒堵在有界队列的磁盘producer；已取消reader在打开源/取得Windows共享锁前退出。取消仍不声称能中断已进入OS的read。
- **验证：** `--lib outgoing_peer_drop --locked` 1通过，确认满队列producer原先受背压、peer Drop后250ms内退出；transfer 24通过，新增取消不打开不存在源文件回归；lib+tests严格Clippy通过。

- [x] **协议14独立来源切片（非整链过渡）：** 新增有界FileTask合同：能力交集、单文件1TiB/聚合16TiB/100万项资源上限、逐项manifest、块摘要/持久确认/恢复证明、暂停/恢复/取消；旧FileOffer仍限制256MiB，输入上下文与Noise算法不变、relay JOIN版本不变。协议13在所有已有带版本载荷边界明确拒绝。`test -p desklink-protocol --locked` 54通过、1个既有benchmark ignored；新增3项行为测试覆盖>4GiB精确数值、截断/尾字节、恶意路径/预算/溢出。此提交只证明共享合同，不代表Windows/Mac功能已接通，主验收保留[ ]。

- [x] **持久块证明与目录磁盘原语切片：** SourceIndex按48KiB块持久化摘要/前缀证明，恢复O(1)定位；CheckpointLedger先sync数据与日志再确认，重启截断未确认尾部，发布前重新校验实际所有块。目录按深度栈流式枚举、队列2/扫描worker8；逐项本地JSONL可保留源身份。Windows固定祖先句柄、拒绝重解析点/ADS/路径越界，暂存独占与create-only发布，保留空目录、中文和长路径。`test -p desklink-windows --lib transfer --locked -- --test-threads=1` 65通过/0ignored（旧传输28、checkpoint10、目录20、恢复7）。涵盖真实>4GiB稀疏偏移、源同长/恢复mtime变化、撕裂日志、离线篡改、满队列取消、旧writer Busy、junction和重名竞态。此切片尚未代表新任务端到端/UI/Mac接线完成。

- [x] **Windows前端独立切片（原生接线另片验证）：** 协商前/不支持时禁用文件与目录入口；按已认证peer scope隔离任务，u64字节/修订号十进制字符串+BigInt，扫描/已发送/已落盘确认/最终校验分别展示。任务窗口最多64项、100ms合并更新、逐项/整任务重试与暂停/恢复/取消；不重建远控canvas。等待队列新增“正在加密保存”而非提前声称已保护；空目录成功/部分成功可打开下载目录。固定Bun1.3.14全套508通过/0失败（73文件、2052断言），输入合同检查/TypeScript/Vite构建通过。此为前端行为夹具（模拟native桥）与构建证据，不替代原生窗口/真机验收。

- [x] **Windows原生任务接线切片：** 双向文件/目录actor接入认证peer和协商预算；持久块确认、暂停/取消/断线与进程重建、显式源重试确认、逐项冲突重试。原生窗口精确取消，队列DPAPI保存移出媒体/输入循环并以代数防止旧任务复活。真实NT句柄相对操作覆盖junction替换和create-only发布竞态，可靠NTFS USN恢复免源数据重读，无令牌则安全重验。原生管理库146通过/1需显式签名资产ignored；协议与Windows详细复跑记录见[验证记录](docs/verification/dl2-030-v14.md)。此提交时扫描失败/发布索引优化尚待后续切片；当前结果见下一项，跨平台主项保留[ ]。

- [x] **扫描失败、恢复与发布索引闭环切片：** 持久失败范围及有界完整键去重，成功项编号保持；失败子树/重解析点修复、精确条目预算和重叠范围重试保留有效兄弟项、避免重发成功内容。原清单读句柄贯穿复制、新清单持原writer发布，源根/子目录和接收叶在握手期间固定；错配/混合缓存、种类替换拒绝。已验证账本在可靠NTFS关闭变化记录下导出索引，固定128KiB缓冲，不重读payload；无可靠令牌仍全文复验。清单六遍实际工作/索引记录分别报告进展，不伪造字节或百分比，分卷检查空间。全workspace 904通过/19显式ignored/0失败，fmt/零警告Clippy、Bun508与构建、Python208和C ABI编译检查通过。最终>4GiB重建续传由干净提交的独立入口及外部指纹证据建立；不宣称真实OS重启、双机或Mac接线完成。详见[验证记录](docs/verification/dl2-030-v14.md)。

### DL2-031 · P2 · 让等待队列中所有文件都能管理

- [ ] 保留紧凑摘要，并允许展开完整队列、逐项移除。
- [x] `fix(DL2-031): expose every queued file with stable keyboard focus`：完整 20 项展开列表、逐项移除、重名按 ID 区分，局部刷新保留展开/滚动/删除后键盘焦点；恢复队列复用相同渲染。Bun 1.3.14 文件队列聚焦测试 13 通过，`bun run build` 通过。
- [ ] 真实 WebView 键盘展开、定位并移除第 17 项验收；当前仅合成宿主验证，未进行真实远端文件传输。
- **证据 / 类型：能力缺口。** `apps/windows-ui/src/controller.ts:1131-1142` 固定 queued.slice(0,4)，只显示其余数量；最多 20 个等待项时，第 5～20 项没有逐项入口。
- **方案：** 完整列表、内部滚动、清楚的当前/等待/暂停状态，恢复队列使用同一界面。20 项无需为形式引入虚拟列表。
- **验收：** 加入 20 项后能直接定位并删除第 17 项，不影响其他项和正在发送项；键盘可完成同样操作。

### DL2-032 · P1 · 文件拖放只作用于明确的远控接收区域

- [ ] 对拖入提示与 drop 使用同一可见区域命中规则。
- [x] `fix(DL2-032): bind native file drops to visible session targets`：统一物理/逻辑坐标和遮挡命中，仅可见远控画布/文件面板可接收；菜单和表单不接收。drag 锁定目标，原生文件准备及命令消费再次核对 worker 代数/stream/设备；文件选择器同样绑定打开前目标。Bun 1.3.14 拖放测试 10 通过、构建通过；Rust 管理库 102 通过、1 已有签名包测试忽略。
- [ ] 100%/150%/200% DPI、窗口缩放及双机真实拖放验收；当前为真实回调的合成宿主与原生目标守卫测试。
- **证据 / 类型：源码缺陷。** `apps/windows-ui/src/controller.ts:2147-2164` 在整个 WebView 监听 native drop，仅检查 runtime connected，没有核对当前视图及 drop position。
- **用户影响：** 后台仍有远控连接时，把文件拖入设置、共享或画布以外区域也会加入远端发送队列。
- **方案：** 仅当前可见远控 viewport 或明确文件面板接受；统一物理/逻辑坐标和 DPI，菜单/表单默认不接受；提交前核对目标设备及代数。
- **验收：** 100%/150%/200% DPI 与窗口缩放下，画布内生效，侧栏、设置、共享、菜单和窗口外不生效；离开连接页无残留拖入提示。

### DL2-033 · P2 · 明确宿主快捷键与远端快捷键路由

- [ ] 同时提供可发现的退出全屏、发送 Esc 和系统组合键操作。
- [x] `feat(DL2-033): expose balanced remote keyboard shortcuts`：保留宿主 Esc 全屏退出及组合输入保护；提供发送 Esc、Win、窗口切换/关闭、任务管理器、桌面和锁屏组合菜单。组合以同 stream 原子队列发送完整 down/up，取消与 IPC 失败仍清理释放；Ctrl+Alt+Delete 明确本机操作。Bun 1.3.14 快捷键/真实动作宿主/dispatcher/IME 38 项通过，构建通过；未做真实输入注入。
- [ ] 真机全屏远端菜单取消、中文输入法及不同键盘布局验收；当前为合成输入/宿主测试。
- **证据 / 类型：能力限制。** `apps/windows-ui/src/controller.ts:334,3799-3806` 在 capture 阶段截获全屏 Escape 并 stopImmediatePropagation；`984-989` 没有发送远端 Esc 的替代入口。
- **方案：** 独立宿主键/组合键退出全屏，或明确“发送 Esc”菜单；组合键菜单覆盖远端常用系统操作，必要时允许配置；不要吞输入法取消键。
- **验收：** 全屏远程应用可关闭菜单/取消对话框，同时本地全屏也可可靠退出；中文组合、键盘布局、失焦及快捷键按下/松开一致。与 DL2-006、007 联合设计。

### DL2-034 · P2 · 账号表单错误后保留可纠正输入

- [x] 为账号表单保存短期草稿，并使重发验证绑定明确邮箱。
- **修复 / 自动验证：** `fix(DL2-034): preserve in-memory account form drafts and resend address`；草稿只在内存，提交失败恢复字段、错误焦点就近定位；成功或离开流程清除密码；重发绑定注册时邮箱。Bun 1.3.14 草稿与真实 submit/resend 合成宿主测试 4 通过，`bun run build` 通过；未进行真实邮件投递。
- **证据 / 类型：源码缺陷。** `apps/windows-ui/src/main.ts:216-218` 没有草稿绑定；`317-325` 校验失败/提交 render 清空字段；`335-338` 注册后显示重发，但 `286-290` 再从已清空的 DOM 读取邮箱。
- **方案：** 邮箱在内存跨模式保留，pending verification email 独立于 DOM；字段错误就近定位；密码仅保存在短期内存并在成功或离开流程时清理。
- **验收：** 密码不一致、网络失败后邮箱仍在；注册成功可直接重发；不把密码写入 localStorage、日志或诊断。

### DL2-035 · P2 · 隔离账号迟到响应与用户后续选择

- [x] 为账号动作增加操作类型、请求代数和视图所有权。
- **修复 / 自动验证：** `fix(DL2-035): bind account persistence and Host restart to operation ownership`；前端捕获动作/邮箱/代数，切模式或本机模式使旧结果失效；原生登录先暂存，accept/finish 分阶段确认，取消时回滚尚未接受的落盘；Host 等待停止后再次复核所有权，保留未登录启动共享。Bun 1.3.14 账号/更新宿主相关测试 11 通过、生产构建通过；`python scripts/run-windows-cargo.py test -p desklink-windows-ui --lib --locked` 98 通过、1 签名包资产测试忽略，含 8 项新增延迟 HTTP、DPAPI 存储、连续重试、旧 logout 与 Host 等待守卫测试。未向真实账号服务写入。
- **提交说明：** 账号实现、TS IPC 与原生 IPC 分别保留为三个 DL2-035 提交（后两笔主题分别为 `complete owned account IPC bridge`、`wire native account ownership commands`），需整体引入；原生桥接同时保留无账号/恢复失败时的受代数保护 Host 启动。此前拆分暂存失败未重写历史，最终链已以 102 项原生测试再次验证。
- [x] 取消失败补修 `fix(DL2-035): keep failed account cancellation visible and retryable`：原生取消/回滚错误向当前视图传播，不再误报本机模式成功或继续重启；旧错误不覆盖后续选择。账号草稿/实际流程/更新宿主 16 项通过、构建通过；真实临时原子文件冲突回归 1 通过，确认保留原会话备份、失败后仍能重试恢复，不使用 logout 补偿。
- **证据 / 类型：源码缺陷。** `apps/windows-ui/src/main.ts:211-223` 请求中仍能切换模式/进入本机模式，`327-349` 和 `295-302` 的完成结果未校验代数。迟到注册可切回登录，迟到登录可覆盖本机模式选择并再次初始化 workspace。
- **原生副作用：** `apps/windows-ui/src-tauri/src/account.rs:169-201` 在登录响应后保存会话，`apps/windows-ui/src-tauri/src/lib.rs:205-214` 随后重启 Host；只丢弃前端结果仍无法隔离这两步。
- **方案：** 请求捕获不可变的动作/邮箱/代数；切换使旧结果失效，必要时取消底层请求；原生持久化和重启 Host 前同样复核操作所有权。对已完成但用户已离开的登录明确处理本机会话，不直接使用会清空远控记录的 account_logout 作为补偿；不要求用户留在等待页，也不重复订阅 workspace。
- **验收：** 可控 Promise 覆盖注册中切找回、登录中进入本机模式、连续重试；旧结果不能覆盖新选择或重启后台共享。

### DL2-051 · P2 · Mac 账号 restore 遇瞬时网络故障即删除钥匙串会话

- [ ] 区分令牌失效与网络/服务不可达，只有前者清除本地会话。
- **证据 / 类型：源码缺陷。** `apps/apple/Sources/DeskLinkAppleCore/AccountClient.swift:127-132` restore 中 me 请求失败后尝试 refresh，refresh 再失败落入外层 catch 执行 `clearLocalSession()`（删除 Keychain token 并置 signedOut）。启动时无网或账号服务瞬时不可达即被永久登出。Windows 侧 `apps/windows-ui/src-tauri/src/account.rs:80-127` 失败仅返回 Err 并保留存储会话，两端行为不一致。
- **方案：** 网络类错误保留会话进入"离线、稍后重试"；仅明确的令牌无效/被撤销才清除；与 DL2-035 的迟到响应隔离一起设计。
- **验收：** 断网启动、服务 5xx、refresh token 过期三种场景行为各自正确；恢复网络后自动或一键恢复登录。

## 6. 服务端与长期运行

### DL2-036 · P2 · 同邮箱并发注册保持幂等和稳定响应

- [x] 把查重与创建改为数据库原子语义，正确处理唯一键竞争。
- **证据 / 类型：复现缺陷。** `server/account/src/services/account-service.ts:36-40` 先查询，await hashPassword 后插入；`server/account/src/db/store.ts:136-143` 普通 INSERT。两个并发同邮箱请求本次返回 202 与 500。
- **方案：** 原子 insert-on-conflict / 明确 unique conflict 结果；用户、设备及验证任务采用一致事务/幂等策略。重复请求返回相同的通用确认，不泄露账号存在性，也不重复轰炸邮件。
- **验收：** 同邮箱并发 N 次无 500、只创建一个用户，邮件/设备副作用有确定上限；不同邮箱并发不被全局大锁串行化。
- **完成 / 验证（2026-10-26，`fix(DL2-036)`）：** 邮箱 `ON CONFLICT DO NOTHING` 与设备、审计、action token、outbox 同事务；密码哈希在事务外。Bun 1.3.14：账号 24 测试及 typecheck 通过；合成文件库双连接 16 次同邮箱并发仅各生成一条用户/设备/审计/token/outbox 和一封邮件；HTTP 并发均为相同 202；入队失败全回滚；不同邮箱可在另一邮件等待期间完成。

### DL2-037 · P2 · readiness 反映真实依赖状态

- [x] 将进程存活与数据库可服务分开检查。
- **证据 / 类型：复现缺陷。** `server/account/src/app.ts:46-56` 固定返回 ready；本次关闭注入的真实 AccountStore 后，`/ready` 仍为 200。
- **方案：** 低成本、带超时的数据库探测和 schema 状态；邮件故障单独暴露为可恢复降级，不让可选账号故障影响本机远控；输出稳定白名单状态。
- **验收：** 关闭/锁住/只读数据库以及未完成迁移时正确非就绪；健康路径低开销；health 不暴露凭据、路径或内部堆栈。
- **完成 / 验证（2026-10-26，`fix(DL2-037)`）：** `/health` 独立存活；`/ready` 校验 schema 及 rollback-only 写探测，SQLite 锁等待预算 25ms，失败 503，仅输出白名单状态。邮件失败显示可恢复 degraded，不影响数据库就绪。Bun 1.3.14：30 项账号测试及 typecheck 通过，覆盖 closed、双连接持写锁、真实 readonly 文件连接、query-only、迁移版本不全和缺表；100 次内存健康探测实测 1.09ms（本机合成，不代表部署性能）。

### DL2-038 · P2 · 邮件任务与账号历史记录具有完整生命周期

- [x] 将邮件投递从用户请求等待中解耦，并补齐失败终态、回收与恢复。
- **证据 / 类型：能力缺口、性能结构。** `server/account/src/services/account-service.ts:158-178` 注册/找回直接 await 投递，`server/account/src/providers/http-mailer.ts:15` 可等待 10 秒；`server/account/src/db/store.ts:302-316` 在 attempts 达 10 后不再查询，但没有明确终态接口；schema 中 session/action token/outbox/audit 没有保留期回收入口。
- **现有保护：** 已有持久 outbox、进程内 single-flight、最多 10 次重试与退避，不能报告成没有重试或无限重试。
- **方案：** 请求原子入队后返回；独立 worker 认领/恢复/去重，暴露 pending/delivered/failed；按用途保留最小审计与刷新令牌重放检测窗口后分批回收；进程关闭时停止定时器并完成资源释放。
- **验收：** 邮件服务慢/宕机时注册响应不等待投递 timeout；第 10 次失败可诊断/受控重投；重启不丢任务；回收不破坏有效令牌和重放检测，长期数据库增长可解释。
- **完成 / 验证（2026-10-26，`fix(DL2-038)`）：** 请求仅事务入队；独立 worker 按条认领 30s 租约、持久计数和所有权校验，保留 single-flight / 10 次 / 退避，补 delivered/failed 终态、崩溃恢复与本地审计重投接口。schema v3 迁移旧终态，provider 错误仅保存固定类别；关闭先停止接收/计时/新认领，再等当前邮件并释放全部固定 SQL statement。每表每分钟最多回收 500 条，outbox 终态 30 天、过期 action token 7 天、审计 90 天；整个 refresh family 最后到期后再保留 30 天，保留所有邮件引用 token。Bun 1.3.14：46 测试 / 368 断言及 typecheck 通过，含慢投递期间 register/resend/forgot 已返回、文件库重启/迁移、双连接租约去重/迟到 ack 拒绝、第 10 次失败或崩溃终态、显式重投、有效 token 与旧 rotated token 重放、清理边界/限量/计划和关闭文件释放。保留期、吞吐/页复用、至少一次投递与部署长稳边界见 `server/account/README.md`；未连接真实 provider 或线上库。

### DL2-039 · P2 · 诊断保留期清理不扫描并锁住全部历史

- [x] 为清理键补索引，并将大批回收移出每次接收事务。
- **证据 / 类型：复现性能结构。** `server/diagnostics/src/store.ts:17-47` 的事件索引只有 correlation/installation；`103-111` 每次 insert 事务内按 received_at_unix_s 删除 events/batches。本次真实 schema 的 EXPLAIN 返回 SCAN events。
- **方案：** 以 received_at 建索引，按时间/数量分批回收，独立低频维护；保留接收原子性和去重；监测 WAL、磁盘及清理耗时。SQLite 文件是否缩小需实际维护策略，删除行不等于空间立刻归还 OS。
- **验收：** 用真实规模合成历史记录验证 query plan、接收 p95、清理锁时间和磁盘上界；14 天保留语义不变，不为性能丢失批次原子性。
- **完成 / 验证（2026-10-26，`fix(DL2-039)`）：** 时间索引、独立维护（每片 1000 行）、批次标记延后至过期事件排空、关闭释放及白名单存储指标。Bun 1.3.14：17 测试 / 73 断言及 typecheck 通过，覆盖真实文件锁恢复、旧 schema 索引升级、接收失败全回滚和去重。25 万保留事件 + 两轮各 25 万过期事件的文件库：计划由 SCAN 改为时间索引；store 接收 p95 341.436ms → 0.331ms，清理分片 p95 29.236 / 34.205ms、max 137.687ms（完整事务调用作为锁时间保守上界），WAL 高水位 176,303,072 → 7,815,672B；第二轮主文件仅长 4KiB，证明该固定负载页复用。原始证据、命令及索引升级/物理空间边界见 `server/diagnostics/RETENTION.md` 和 `retention-benchmark-evidence.json`；本机合成不含 HTTP/签名、生产长稳或任意输入磁盘硬上限。

#### DL2-044 · 服务端诊断契约切片（整体仍由媒体 / 产品验收）

- [x] 核实 producer `a599246535b73f57e0425cbbfd18c7156e050bba` 的 `DiagnosticEvent::HostMedia(CursorSend)`；仅为 `host_cursor_send` 接受精确 9 字段、host source / warning / 固定 `capability_or_size` reason，三个新增字段不向其他事件开放。无需存储迁移。
- [x] `fix(DL2-044)` 服务切片：保留完整 payload / 批次原子性与去重；分析仅生成一次可选光标通道停用 warning，保留独立视频健康证据，不累加跨 peer 快照、不把 send_errors 转为真实丢包/送达率或主机停止。
- [x] 实际 rustc 1.97.1 经仓库 Cargo wrapper、producer 锁版本 `--locked --offline` 编译原 encoder 生成四组 JSONL fixture；六份输入 blob / SHA-256 已复核，独立重跑除 timestamp 外一致。Bun 1.3.14：54 测试 / 665 断言及 typecheck 通过，包含原 Rust JSONL → Ed25519 签名批次 → 验签/schema → app/store → analysis、混合批次、去重、错误字段/类型/source/reason/签名和 safe integer / u64 边界。
- [ ] GUI observer 绑定 / 导出、真实硬件与媒体链路整体验收由产品 / 媒体工作面继续；本项不能关闭整个 044。045 listener_closed 已按后续真实 SHA 接线（见下方服务切片）；桌面上传器对永久 400 的重试生命周期仍不由本地 server 回归证明。
- **契约 / 证据：** `server/diagnostics/HOST_MEDIA.md`、`fixtures/host-media-cursor-producer.json`、`scripts/generate-host-media-contract.py`。仅运行隔离编码 harness 和本地签名 / 数据库测试，未编造 GUI 已接通或远端送达证据。

#### DL2-045 · 服务端直连监听诊断契约切片（整体仍由媒体 / 产品验收）

- [x] 核实 producer `6dbe53d64c60bbdd50af4b592ebef7471b800fcc`（区别于 98dd 前置测试）；仅为 `host_direct_listener_stopped` 接受精确 7 字段，host source / warning / 固定 `listener_closed` reason / 严格 boolean `retired_active`，不向其他事件开放新字段。存储无需迁移，044 光标契约保留。
- [x] `fix(DL2-045)` 服务切片：新增一次固定 listener 终止 warning；保留独立健康视频/光标证据，不判主机或整个会话停止，不推导网络丢包、中继已恢复成功或重复 peer 次数。retired_active 两值都合法，仅反映是否从 slot 取出选定连接。
- [x] 原 Rust encoder 经仓库 wrapper / rustc 1.97.1 / `--locked --offline` 生成四组真实 JSONL fixture；六份输入 blob / SHA-256 已复核。Bun 1.3.14：诊断 61 测试 / 868 断言及 typecheck 通过，覆盖 producer → 签名/验签 → 固定 schema → 混合 cursor/legacy/listener 批次入库/去重 → analysis，以及坏字段/类型/source/reason、签名篡改和 safe integer/u64 边界；原 cursor fixture 字节未改且其完整回归仍过。
- [ ] 产品 observer 接线 / 导出、真实 listener 生命周期与 relay 回退验收继续由媒体 / 产品工作面处理，不能据 server 契约测试关闭整个 045。未知未来 schema 仍需真实 producer SHA；永久 400 的桌面重试策略不在本服务验证内。
- **契约 / 证据：** `server/diagnostics/HOST_MEDIA.md`、`fixtures/host-media-listener-producer.json`、`scripts/generate-host-media-contract.py --event direct-listener`。仅隔离编码 harness 和本地合成签名/SQLite，未连接线上。

#### DL2-044 / DL2-045 · 共享完整 u64 诊断精度切片

- [x] 对齐实际 producer `28c11ee05a08183f7636ede7946e93029c7bbb16`：仅已确认 7 事件 / 18 个 u64 键接受 canonical 十进制字符串（至 `18446744073709551615`），兼容历史 safe-integer 数字；stream 非零，counter / u64 duration 可零，cursor errors ≤ attempts 精确比较。timestamp / u16 / u32 / Duration 数字字段、HostMedia 精确键 / source / reason / boolean 边界不变；未知字段/字符串不扩散。
- [x] payload 仍存 TEXT JSON，无 schema 迁移；分析用精确本地整数与 canonical stream key，保留相邻大 ID、max / sum / threshold 和超过单个 u64 的汇总；小报告值保持 number，大值输出精确十进制字符串，BigInt 不进入 JSON。
- [x] 同路径原 Git blob 导入唯一 `apps/windows/tests/fixtures/diagnostic-u64-producer.json`（`58f386235e9dda4c5045b21af9a1a37a53cc39da` / SHA-256 `8d7d3124436b52906c63905340d7fa7a968017de40a639db37efe18489bb19bf`），8 份源指纹均匹配 producer；不改写、不重签。6 当前 + 6 历史升级原始 Rust body/hash/签名 → 真实 loopback HTTP → 验签/schema → 文件 SQLite/重启/去重全部通过；旧 unsafe numeric 原签名批次仍整批 400。原 cursor/listener fixture 字节未变。Bun 1.3.14：诊断 129 测试 / 6017 断言及 typecheck 通过；详见 `server/diagnostics/U64_CONTRACT.md`。
- [ ] GUI observer / 导出、真实设备和媒体恢复及最终联合候选验收继续保留；null correlation 样本不伪造真实会话，单独合成分组只验证已落库数值分析。本切片不能关闭整个 044 / 045。

### DL2-054 · P2 · 诊断接收在验签前限额，持久配额限制留存增长

- [x] 在读 body / 验签前按 socket / 显式可信代理 IP 和全局预算准入，有界键状态；为事件、批次、全部可变 TEXT 字节设事务持久配额。
- **复核：** 039 仅修复清理；安装签名仍是自认证，换密钥不能算新准入额度。新增全局/IP 请求、字节和并发预算，验签后再加 installation/IP/全局事件预算；默认不信任转发头，systemd 模板显式信任 nginx loopback 覆盖的 X-Real-IP，溯源存真实 IP。
- **实现（`fix(DL2-054)`）：** O(1) 有界状态，满额拒绝新键；SQLite 配额计数/触发器与接收同事务，计费全部复制元数据 UTF-8 字节，去重不重复计费，清理归还额度，超额整批回滚并 503。
- **验证（2026-10-26）：** Bun 1.3.14：45 测试 / 453 断言及 typecheck 通过。真实 loopback socket 伪造头轮换后读 body 前 429；200 新密钥/IP 在 8 条持久配额下只写 8 条；1 万键洪泛上界、超时释放、Unicode/元数据计费、两连接/重启/旧库回填、满额去重、零新增事件批次上限均通过。配额后 1000 事件清理基准烟测通过。部署、默认配额和物理 WAL/磁盘边界见 `server/diagnostics/ADMISSION.md`；039 的 25 万性能证据明确是本项触发器前快照，未冒称当前百分位。

### DL2-055 · P2 · 账号限流使用可信 socket / 代理来源且容量有界

- [x] 拒绝以客户端自带 XFF / CF 头创建独立 IP 桶，限制限流键数和每次清理工作量。
- **复核：** 036～039 后的 `enforceRateLimit` 仍取 XFF 最左项，原 RateLimiter 在有效窗口内无键数上限；038 已将邮件移出请求，不能再描述成同步外发。
- **实现（`fix(DL2-055)`）：** Bun 注入 socket peer；默认不信任转发头，精确配置 `DESKLINK_ACCOUNT_TRUSTED_PROXIES` 后仅接受直属代理覆盖的单值合法 X-Real-IP。最多 16,384 桶，每次最多检查 16 项过期状态；满额拒绝新键，不淘汰有效耗尽额度。
- **验证（2026-10-26）：** Bun 1.3.14 账号 49 测试 / 397 断言及 typecheck 通过；直连/可信反代/缺 socket context、伪造 XFF/CF/X-Real-IP、IPv4-mapped peer、1 万随机键洪泛与额度保持回归通过。未部署代理；部署时须按 README 配置可信精确 peer、覆盖头和阻断外部直连。
- [x] 独立修复继承的缺失邮箱身份桶问题（`fix(DL2-055)` 后续提交）：真实复现 11 个不同 IP / 不同账号 / 有效 reset 与 verify token 的第 11 次均 429。现只有真实邮箱动作的合法规范化邮箱使用邮箱桶；token 动作忽略 body.email，保留原可信 IP 额度并将 GET/POST verify 纳入同一全局 120/min 前置预算。定向 9 测试 / 383 断言、完整账号 79 测试 / 1072 断言及 typecheck 通过，覆盖跨身份互不耗尽、同源额度、GET/POST 共桶、121 次前置拒绝且 token 未消费、email 注入隔离与合法邮箱限额保持；未连接线上/provider。

### DL2-056 · P2 · 均衡账号发现路径并限制未认证计算资源

- [ ] 完成部署形态下的时序枚举验收。
  - [x] 注册重复/新地址均一次 hash，登录缺失/存在均一次 verify，找回/重发均一次 dummy verify；请求保持 outbox 解耦，统一 100～124ms 最低响应时间窗。
  - [x] 增加预读 body 的全局 120/min 配额、16KiB / 2s body 边界、全进程 2 个 Argon2 工作 + 16 个等待上限和溢出 429；真实 gate 回归确认两个账号分支都等待计算。
  - [x] Bun 1.3.14 本地每分支 100 样本计时：login/register/forgot 排序 AUC 0.5164 / 0.5794 / 0.5664；复现了仅均衡哈希仍有残余差异并加响应时间窗。10 项定向隐私/注册/密码测试及 typecheck 通过，完整账号集 59 项通过（含并行 058）。修复引用 `fix(DL2-056)`；方法、前后原始结果和资源预算见 `server/account/PRIVACY.md`。
  - [x] 计时证据 JSON 换行归一后解析值逐项相同，提交范围 whitespace 检查通过（`chore(DL2-056)`）。
  - [x] 修复预算引入的密码重置生命周期回归（`fix(DL2-056)` 独立后续提交）：基线真实复现满 2 个工作 + 16 个等待时 reset 429、排空后同 token 却 400。现仅只读预检，hash 成功后取得 IMMEDIATE 写锁并以新时钟重验/消费，密码、会话撤销和审计同事务；hash/DB 失败不误消费。Bun 1.3.14 定向 9 测试 / 74 断言，完整已跟踪账号集加本项回归 70 测试 / 689 断言及 typecheck 通过，含 429→排空→同 token 204、双连接仅一次成功、hash 拒绝、密码/审计写失败全回滚、过期/撤销竞态与锁忙后重试。无效 bearer token 同 400 且不占 hash，不新增邮箱枚举路径；未声称废除同账号其他独立重置链接。
  - [ ] 部署形态网络与空闲/饱和负载对抗计时。有限本机样本不能证明任意负载下统计不可区分，CPU 工作超过响应窗时仍须实测；本次不连接线上库/provider。

### DL2-057 · P2 · 服务关闭与启动任务拒绝处理

- [ ] 完成所有服务的实际信号关闭验收（relay 部分由网络工作面处理）。
  - [x] 复核 038 已捕获账号启动 worker 拒绝，038/039 已加两个 Bun 服务信号路径，无须重复实现旧 `void flushOutbox()` 缺陷。本提交 `fix(DL2-057)` 提取生产调用的幂等 runtime，关闭错误受控处理，补诊断固定 SQL/native handle 确定释放和初始化失败清理。
  - [x] Bun 正常关闭使用 `stop(false)`：拒绝新连接并排空已接收请求后关库；`stop(true)` 实际会取消在途请求，不能据草稿误当排空。邮件持久待发任务留待重启，不停机无限 flush；诊断 systemd 模板显式 20s 关闭窗口。
  - [x] Bun 1.3.14 完整账号 61 测试 / 615 断言，诊断 47 测试 / 463 断言及双方 typecheck 通过。真实 loopback socket 回归覆盖两个服务拒新连接、在途请求/签名批次成功后才关库、重复关闭单次执行、worker 启动拒绝无 unhandled rejection、定时器停止与文件无需 GC 释放。
  - [ ] 部署系统实际 SIGTERM/SIGINT 交付、停止预算与故障负载验收；当前为 Windows 本地 runtime/socket 验证。
  - [ ] relay 主动 QUIC CONNECTION_CLOSE、在途任务与对端及时重连验收；本工作面未修改 relay，也未据 Bun 测试关闭整项。

### DL2-058 · P3 · 将账号设备标签隔离，消除跨账号登录阻塞

- [x] 保留正常退出换号，同时使持他人 deviceId 的注册/登录无法改写或阻塞原账号登记。
- **复核：** 真实 app 合成复现 A 退出后 B 使用同 ID 注册即可使 A 登录 409；没有获得 A 权限，属于可用性排他锁，不是账号接管。Windows/Apple 均持久复用安装标签，因此直接禁止跨用户 ID 会破坏正常换号。
- **实现（`fix(DL2-058)`）：** `account_devices` 复合主键 `(user_id,id)`、session 复合外键；查询/撤销按账号作用域，客户端请求和响应 ID 不变。schema v4 迁移逐句执行并校验旧/新外键和会话数，保留 token/family/重放窗口；跨 owner 旧历史补已撤销占位，拒绝坏源，失败关库并全事务回滚。
- **验证（2026-10-26）：** Bun 1.3.14 完整账号 59 测试 / 605 断言及 typecheck 通过；包含正常 A/B/A 换号、未验证注册不能阻塞原账号、session 跨账号外键拒绝、文件库迁移/重启/旧令牌刷新和重放、坏源及后段 DDL 失败全回滚。未修改桌面身份/远控协议，未连接线上库。

### DL2-052 · P0 · 收紧 relay 未使用流与资源预算

- [x] **第一波合并接口适配：** Apple真实小窗口背压夹具与网络资源预算统一为typed flow-control入口，只允许三窗口逐项不超过预算，普通bind保留默认；uni/bidi/datagram/crypto与生命周期限额继续由relay唯一执行，超预算拒绝。实际目标回归relay边界2、transport flow2、真实direct静默/持续收包2及Python worker合同5通过；完整候选门禁继续待实跑，不以局部通过关闭整个集成。

- [x] 在精确源码 `020c5b3` 的本地候选容器完成负载实测：256MiB /0.75CPU /PIDs64、64 live connections、480 started writers，cgroup 峰值 34402304B（32.81MiB）、OOM kill=0；四种 join 前 datagram 均及时关闭，独立健康会话保持可达。只证明已记录负载，不把载荷模型或客户端写完成数当 RSS 保证/服务端完整收帧数。主题 `test(DL2-052): verify candidate container resource limits`，完整 JSON/镜像 ID/日志 hash 见资源记录。
- [x] 核实锁定 quinn-proto 0.11.16 的 uni=100、单流 1250000 字节、连接 VarInt::MAX 等默认；代码禁用 uni，显式限制 bidi/stream/connection/datagram/crypto/incoming 额度。默认连接/session 改为 64，声明 160MiB 缓冲预算，包含 accepted/rejection/incoming 的默认模型为 152MiB；超预算配置启动拒绝，候选 Compose/Dockerfile 与 256MiB 容器限制对应。未部署。
- [x] 核心提交的本地真实 QUIC 资源夹具、最大可靠帧、慢接收端隔离通过；当时 relay 共 38 项常规测试，Clippy all-targets `--locked -- -D warnings`、fmt/diff 检查通过。Linux RSS/cgroup 只读观测和 CLI 已加入；后续容器实测见首项。主题 `fix(DL2-052): bound relay QUIC buffers and validate admission budgets`，细节见 [relay 资源边界](docs/relay-resource-bounds.md)。
- [x] Join 前 datagram 防护：依赖按载荷长度计队列字节，空 datagram 的元数据不能靠字节窗口限制；整个 join 阶段现拒绝首个任何 datagram，并取消未完成 join/登记。空/非空×未join/partial join 四种真实 QUIC 回归修复前均红、修复后全绿；relay 62 常规用例及 Clippy/fmt/diff 通过。最新候选 `020c5b3` 已复测，未用初轮 e0 镜像替代本防护证明。主题 `fix(DL2-052): reject datagrams before relay join completes`。

### DL2-053 · P1 · 为 join 与已开始的可靠帧建立应用层截止时间

- [x] 默认 8 秒绝对 join 预算覆盖 admission 后 TLS、accept、请求与回复；超时关闭并回收名额，已 attach 但回复被零窗口阻塞的 participant 也由 guard 清理。新增可配置、非零验证的 join/partial-frame deadline。
- [x] 默认 30 秒限制不完整 routing header；后续每帧在首个 length 字节后才启动共享 length/body 的绝对截止时间，滴字节不续期、超长在分配前拒绝；认证可靠 lane 正常帧间空闲和完整帧后的 EOF 不断开连接。
- [x] 新增 6 个 helper 单元及 7 组真实 QUIC deadline 回归；relay lib/bin/deadline/resource/session 共 51 项常规测试、对应 Clippy `--locked -- -D warnings`、fmt/diff 检查通过。512 静默连接本机 probe 已运行，全部有 keepalive PING、最终 admission/worker 为 0；client-connect→应用关闭 p95/p99 为 8020.018/8024.498ms，整个批次至清零 8577.337ms。512 夹具显式声明 2GiB 缓冲预算，未修改生产 64 连接默认或容器配置。
- **修复主题：** `fix(DL2-053): expire incomplete relay joins and reliable frames`。本机合成验证范围与测量口径见 [relay 读取截止时间](docs/relay-read-deadlines.md)；未做公网探针、部署或真实输入注入。

### DL2-057 · P2 · relay 受控停机与任务收尾（Rust 切片）

- [ ] 汇总 Bun 服务关闭与实际 Windows Ctrl+C 验收后，再关闭整个 057；Rust relay、Linux 真实 child 与候选容器 SIGTERM 已验证。
- [x] relay 注册 Unix TERM/INT 或 Windows Ctrl+C，close 停止新 admission 并主动关闭 QUIC；JoinSet 管理连接、拒绝、可靠子任务与媒体，总收尾预算默认 7 秒并等待 endpoint idle，超时明确报告 forced/timed_out，metrics task 取消等待。两个 CLI 检查在注册信号前直接返回。
- [x] 真实 loopback 覆盖两条可靠流背压时只关闭源、目标仍活/健康会话继续、混合认证/未 join/媒体/TLS 拒绝任务、重复 close、ApplicationClosed、新 admission 不可用和超时分类；58 项常规回归、对应 Clippy `--locked -- -D warnings`、fmt/diff 检查通过。Tokio 只启 signal，锁文件只补已有包依赖边，无版本升级。
- [x] Linux 同源 `020c5b3` 真实 child SIGTERM 用例 1 通过；同源静态候选容器完成资源负载后，实际 docker stop 触发 SIGTERM，正常 peer 收到 relay shutdown、退出码 0、graceful 各计数归零、endpoint idle。日志 85ms 仅为服务端收尾阶段，不是 Docker 命令端到端耗时；镜像 ID 与原始日志 hash 见关停记录。
- **修复主题：** `fix(DL2-057): drain relay tasks and notify peers on shutdown`。任务生命周期与 052 缓冲预算的关系、实际验证和 Linux 执行命令见 [relay 受控停机](docs/relay-graceful-shutdown.md)。未部署。

### DL2-052 · P0 · relay 未收紧 QUIC 单向流与流控窗口，少量未认证连接即可 OOM

- [ ] 显式禁用未使用的 uni 流并收紧流控窗口，使对端可请求的缓冲上界与容器内存匹配。
- **证据 / 类型：源码缺陷（公网可达）。** `server/relay/src/lib.rs:868-872` TransportConfig 只设置 keep_alive 与 max_idle_timeout；全仓无任何 `accept_uni()` 调用（业务只用 bidi 流与 datagram），而 quinn 默认值（已对照本机 quinn-proto 0.11.16：`max_concurrent_uni_streams=100`、`stream_receive_window≈1.25MB`、连接级 receive window 未收紧）会作为初始流控额度在握手时直接下发。
- **影响：** 攻击者只需完成 QUIC 握手（无需通过 join 认证）并打开 uni 流写入数据，服务端将永久缓冲无人读取的内容；按默认窗口估算单连接可积累约 100MB 量级未读缓冲，而生产部署 `deploy/tencent-relay/compose.yml:35` 为 `mem_limit: 256m`、`:14` `DESKLINK_RELAY_MAX_CONNECTIONS: "512"`——少量连接即可 OOM-kill 公网 relay，影响全部会话。精确倍数在修复时以实测复核。
- **方案：** `max_concurrent_uni_streams(0)`（协议不使用 uni 流，quinn 官方建议直接置零）；按可靠消息 64 KiB / datagram 1200 B 的实际上界收紧 stream 与 connection receive window；增加内存水位观测。
- **验收：** 恶意客户端开满 uni 流灌数据时 relay 内存有界且连接被关闭；正常 bidi 业务不受影响；容器内实测峰值内存与配置有明确对应关系。

### DL2-053 · P1 · relay join 与转发读取无应用层超时，静默连接长期占用 admission

- [ ] 为 join 阶段与转发流读取增加空闲超时。
- **证据 / 类型：源码缺陷。** `server/relay/src/lib.rs:970-973` `accept_bi().await` 与 `read_request` 无超时；`forward_reliable` 的 `read_exact`（`1176,1195`）同样无超时。传输层 keepalive 5s / idle 15s（`crates/desklink-transport/src/lib.rs:29-30`）会被客户端协议栈对 keepalive 的自动 ACK 重置——"已连接但应用层不发一字节"的连接永远不会被清理，`try_reserve_connection` 的 admission 名额（`lib.rs:928,949-955`）被无限期占用。第一阶段只给客户端加了 join 8 秒超时，服务端没有对应保护。
- **方案：** join 的 accept_bi/read_request 包 8-10 秒超时；转发流读取空闲超时可放宽至 30-60 秒；超时释放名额并记录。
- **验收：** 静默连接在超时后被清理、名额可用；慢速正常会话不被误伤；512 上限下的清理延迟有实测数据。

### DL2-054 · P2 · diagnostics 限流可被自认证身份绕过，且限流发生在验签之后

- [ ] 限流前移到读 body 之前并增加不可伪造维度；为存储增长设总量配额。
- **证据 / 类型：源码缺陷。** `server/diagnostics/src/index.ts:62-64` 先执行 `arrayBuffer()`、blake2s、ed25519 验签与 JSON 解析，之后才按 `installation_id` 限流（12 次/60 秒，`index.ts:40`）；而 `installation_id` 由请求自带公钥哈希派生（`server/diagnostics/src/validation.ts:120-126`），签名校验只保证"签名与 ID 自洽"（`153-155`），没有准入名单——换一对密钥即获得全新身份，官方探针本身就每次生成新身份。nginx per-IP 30r/m（`server/diagnostics/deploy/nginx-location.conf:1`）是当前唯一有效约束，跨 IP 即失效。`SlidingWindowLimiter` 的清理（`index.ts:24-30`）在 size>10_000 时全表扫描。另外 `index.ts:67-68` 存库的 `remote_address` 在反代部署下恒为 127.0.0.1，取证字段失效。
- **方案：** 读 body 前按受信代理头的真实 IP 限流；installation 维度增加事件/字节总量配额；限流键设上界与 O(1) 淘汰；remote_address 读取代理注入的真实 IP。
- **验收：** 换密钥对洪泛不再无限写入 SQLite；单 IP 与全局配额生效；生产形态下 batches.remote_address 可用于溯源。

### DL2-055 · P2 · account 限流 IP 取 XFF 最左值可伪造，限流桶无界增长

- [ ] 改用不可伪造的 IP 来源，并给限流表设键数上界。
- **证据 / 类型：源码缺陷。** `server/account/src/app.ts:163-165` 取 `x-forwarded-for` 的第一个元素——nginx `proxy_add_x_forwarded_for` 是追加语义，客户端自带的 XFF 排在最前、完全可控，`register 5/h、login 12/min、forgot-password 5/h` 等 IP 维度限流全部可绕过（结合 DL2-038 的同步投递可无限消耗外发邮件配额）。`server/account/src/domain/rate-limit.ts:8-27` 桶仅在"新建桶且 size≥256"时清理过期项，伪造随机 IP 可在窗口内造成 Map 无界增长。
- **方案：** 仅信任代理覆盖的头（如 X-Real-IP）或 rightmost-untrusted 策略，按部署形态配置；限流表加最大键数与 LRU 淘汰。
- **验收：** 自带 XFF 不再获得独立限流桶；伪造 IP 洪泛下内存有界；直连与反代两种部署限流均有效。

### DL2-056 · P2 · account 登录、注册与找回存在时序型用户枚举

- [ ] 统一各分支的计算成本与异步形状。
- **证据 / 类型：源码缺陷。** `server/account/src/services/account-service.ts:72-75` `!user || ... || !(await verifyPassword(...))` 短路使"邮箱不存在"跳过 argon2id（几十至几百毫秒差异）；注册在 `36-38` 用户已存在时立即返回；找回在 `88-89` 用户不存在时立即返回，存在时却要等待最长 10 秒的邮件投递，差异更大。响应文案已统一，但远程计时可稳定区分邮箱注册状态。
- **方案：** 邮箱不存在时对固定哑哈希执行一次 verify；找回统一走 outbox 异步路径（与 DL2-038 一并消除投递等待差异）。
- **验收：** 大样本计时无法区分"不存在/存在密码错"分支；注册与找回接口同理；不泄露账号存在性。

### DL2-057 · P2 · 三个服务无优雅关闭；outbox 启动 flush 是未处理拒绝

- [ ] 增加信号处理与受控停机，修复未处理的 Promise 拒绝。
- **证据 / 类型：源码缺陷。** `server/account/src/main.ts:19-21` 启动路径 `void app.flushOutbox()` 没有 `.catch`（紧邻的定时器路径有），SQLite 异常会成为未处理拒绝。三个 Bun 服务与 relay 均无 SIGTERM/SIGINT 处理：diagnostics/account 的 `store.close()` 永不被调用，relay 的 `RelayServer::close()`（`server/relay/src/lib.rs:910-913`）无任何调用方；`deploy/tencent-relay/compose.yml:46` `stop_grace_period: 10s` 之后直接 SIGKILL。
- **影响：** 滚动重启时在途 SQLite 事务与 WAL 不落盘；relay 不发 QUIC CONNECTION_CLOSE，对端只能等自己的 15 秒 idle 超时——正是客户端 8 秒 join 超时要应对的黑洞窗口。
- **方案：** Bun 侧 `server.stop(true)`、最终 flush 与 `store.close()`；relay 注册信号后调用 `relay.close()` 并等待在途任务收尾；文档化停机预算。
- **验收：** SIGTERM 后新请求被拒、在途请求完成、数据库正常关闭；relay 主动关闭使对端立即进入重连而不是等超时。

### DL2-058 · P3 · 已注销设备登记可被其他账号静默接管

- [ ] 跨用户改绑需要额外验证或直接拒绝。
- **证据 / 类型：源码缺陷（前提：deviceId 已泄露）。** `server/account/src/db/store.ts:170-177` 非 revoked 的他人设备返回 409，但 revoked（登出即标记，`server/account/src/services/session-service.ts:122`）的他人设备走 UPDATE 静默改绑并清空 revoked_at；随后原主人在本机登录反而命中 409，在其主动登出前无法使用。
- **方案：** 设备行归属不可变，换号插入新行；或跨用户改绑要求邮箱确认；至少"revoked 且属他人"也返回 409 并引导重新注册设备。
- **验收：** 持他人 deviceId 无法静默改绑；正常"退出后换账号"流程不受影响。

## 7. 代码整理与验证体系

本节保留工程各阶段的原始来源与报告；“当时/当前未接线”、零 producer、旧 API 调用及失败计数仅描述各条绑定的工程快照（包括 `5501aad`、`56f53b4`、`d6f153e`），不外推为当前候选状态。当前候选已保留 J 的 Windows 非零生产者、完整 A 与 N 的已提交切片；工程 040 实际共享错误策略的独立代码/验证切片另列，整合后仍须实际复跑。合并、既有来源报告和本次文档裁决均不等于候选联合测试已通过。

### DL2-040 · P1 · 按实际所有权拆分巨型控制模块

- [ ] 将协议/会话策略、平台能力、媒体、输入、传输和 UI 生命周期分开，消除重复实现。
- **证据 / 类型：维护结构与已发生契约漂移。** 当前 `apps/windows-ui/src/controller.ts` 3854 行，`apps/windows-ui/src-tauri/src/controller.rs` 6028 行，`apps/windows/src/runtime.rs` 3869 行，`crates/desklink-ffi/src/lib.rs` 2169 行。行数本身不是 bug；DL2-001、003、007、025 已证明跨层策略/声明实际漂移。
- **方案：** Rust 共享 session/transport/policy 持有恢复、能力和背压规则；平台 adapter 只做捕获、注入、存储、权限；前端分为 session store、media presenter、input、transfer、shell。统一错误 code/状态/命令结果，减少依靠字符串和裸整数分支。
- **实施方式：** 随真实修复完成纵向切片迁移，删除被替代路径；允许直接调整内部 API/协议，但不把重命名目录或更换框架当作性能收益。用 CodeGraph 确认调用方，再以编译和测试验证。
- **验收：** 一项策略只有一个所有者，生产调用与测试调用相同；Windows 和 Apple 使用同一语义；每个新边界具有资源/取消/错误合同；通过 DL2-014 的前后性能比较。
- [x] **不冲突的公共合同整理（`docs(DL2-040): define boundary ownership and integration gates`）：** 新增 `docs/second-stage-boundary-contracts.md`，经 CodeGraph 查询/调用影响和本 worktree 源码逐项确认 SessionMachine、ReconnectSchedule、三处 transport 分类、两套 controller ConnectFailure、同步 join 和 ABI/Swift 映射。保留 persistent/临时邀请、host 服务/单次 controller 的语义差异，明确命令接纳/完成/终态、取消/资源/callback 所有权和稳定错误类别，记录已提交跨组接口与固定 SHA。
- **本切片验证：** 文档本地链接、10 处相关源码路径和 12 个完整提交身份可解析，独立只读复核无阻塞事实错误，`git diff --check` 通过；未修改 crates/apps/server 业务源码。既有 Windows 全栈验证保留原始源码身份，不冒称各组集成验证或新增性能结果。
- [ ] **仍依赖集成：** 009 协议13、Apple 003/004/025、媒体011/012/021与产品002等提交须在第一波稳定revision收口；网络024生产接线和文件协议14后置。稳定transport分类已独立迁移，其他controller input/presentation/media_generation与FFI输入发送所有权待Apple当前013稳定；HostError::Transport(String)须独立类型化，不能套Controller规则。DL2-014同硬件比较、Swift/Xcode、资源/取消/权限场景仍缺，040主项未完成。
- [x] **追加协调合同（`docs(DL2-040): record input epoch ownership and probe lane gates`）：** 明确 Apple/FFI 单一 owner 发布 `VideoConfig.input_epoch` / `InputEnvelope{stream_id,input_epoch}`，媒体 host 原子清键/推进/映射锁校验，产品实际首帧呈现后启用并在入队固定 context；旧输入/旧 release 必须隔离，取消与本机收回独立有效，不以 seq/wallclock/config_version 替代。013 严格绑定仍未完成。已核对媒体独立 probe lane 提交 `0836a8b311a858bf12f0d0b4427961c35f5986a7`；合同、调用方及接入验证须分别交付。文档链接、固定提交身份与 diff 检查通过，未改活跃模块。
- **合同导入前的字段记录（`docs(DL2-040): clarify pending framed input context contract`）：** 记录 owner 明确的字段顺序、非零 stream/epoch、显式 InputContext 和 `[0, PROTOCOL_VERSION:u16 little-endian]` 前缀，由 encode_input/decode_session_input 唯一处理；禁止旧合法 Function(1) 的布局歧义、无context fallback和发送时补当前值。当时字段/前缀尚未落本分支；首帧门禁适用于该context远端输入，取消/本机收回独立。
- **合同导入后的实际边界（`docs(DL2-040): separate landed wire contract from zero-context producers`）：** 来源 `bc6c0a62d3817fbe44759cddf5069c3dee117f4c` 已复用为 `5501aad9f7d160f751048036ac2a14cf0a315680`，只有wire/codec合同落地。HostVideoPipeline、FFI host配置以及两处controller输入生产者仍显式零；显式发送API/host映射/两平台生产者待接线。真实host runtime在prepare生成配置处被拒绝InvalidVideoConfig，非夹具缺字段；当前不可交付，不默认1、不在发送时取当前epoch、不跳过业务失败。
- **会话新鲜度与u64合同（`docs(DL2-040): require fresh Noise epochs and lossless IPC identities`）：** 只读确认HostConnectionSettings.stream_id持久化、supervisor仅run内推进，撤回“新stream可固定1”假设。每次新认证会话/Noise用OsRng非零随机起点（同进程也重新分配），仅几何/活动输出/有效性边界checked推进，纯编码不变；RNG/零/溢出明确失败或重建，无默认1/wrap/round/clamp。Windows身份IPC规范十进制字符串+BigInt完整u64，C/Swift UInt64；epoch不作key/nonce。记录为待接线合同，不冒称当前sender或host已实现这些规则。
- **真实帧配置配对（`docs(DL2-040): preserve config consistency while pairing geometry epochs`）：** 帧/mailbox无独立epoch，host同会话epoch变化必须推进真实config_version并发布配置/恢复关键帧，同尺寸/原点改变也覆盖；纯编码可只变version不清键。源码已有同stream+同version任一VideoConfig字段变化→InconsistentVideoConfig（Eq含epoch），保留此拒绝，不描述成已复现旧帧误激活；禁止放宽校验或重贴旧buffered输出，正确新配置+对应帧实际呈现后才放行。
- [x] **实际共享错误策略拆分（`refactor(DL2-040): centralize role-aware transport retry classification`）：** transport纯模块唯一穷尽分类，Windows host/FFI controller删除重复match并按显式上下文委托；保留Malformed/SessionNotFound/SessionOccupied三项Host可重建、Controller永久失败差异，persistent/预算/取消和HostError(String)不变。CodeGraph定位后核对工程/Apple880e/网络a41原函数一致；真实报告transport-policy/report.json源f8a89d6+dirty切片：Rust1.97.1、隔离target经wrapper，transport29/Windows策略18/FFI lib12通过0忽略、三包lib/tests严格Clippy及workspace fmt通过。两个生产入口与全部错误/rejection双角色覆盖；未改wire/ABI、不声称整体040或性能完成。
- [x] **当前第一波实际职责收口（2026-10-26，测试源a4c5b40）：** 工程完整来源6b31360已merge，transport策略单一owner与Windows Host/FFI Controller两生产委托在完整J+A/CB+N+S+D候选中保留，三项Host/Controller差异、persistent/预算/取消与HostError::Transport(String)现状不变。干净 `a4c5b404d43794d646394923a481f23c9298c3ed` 的fast/integration 25检查、all-targets/all-features check、严格Clippy与fmt通过，完成该真实策略拆分的第一波接入；其余职责拆分和同条件性能比较仍未完成，040主项保持 `[ ]`。证据见[第一波集成记录](docs/wave1-integration.md)。
- **历史来源状态：** 本节此前“合同尚未落地、零生产者、待完整Apple/候选接入”的记录属于各自明确源码时点，不代表a4当前状态；保留原失败与未验范围。013的完整代码/Rust本地切片现已通过，Swift Recording/实际呈现拓扑仍待验；GPU235与文件协议14后置，不扩大本次040修改范围。

### DL2-041 · P1 · 验证入口覆盖全栈与真实跨层边界

- [x] **Windows真实C ABI环境修复（`fix(DL2-041): initialize MSVC for native verification subprocesses`）：** Cargo wrapper调用的native helper先复用有效developer环境，否则定位VS并只在内存捕获/验证x64 cl与标准头环境，再保留Ninja/CRT策略；不输出整环境。17项回归含真实cmd空格路径通过；当前整合源码实际FFI91通过0忽略、真实MSVC C布局和双头检查通过（merged-ffi-msvc.json，源a97f2d4+dirty环境切片）。初轮缺cl失败报告保留，不跳过ABI；源码哈希提交后绑定。第一波最终服务/诊断闭环及整套门禁仍待完成。
- [x] **发布背压门禁与测试夹具一致（`test(DL2-041): bound integration observer queues`）：** 首轮完整候选29643e9的Rust/Bun门禁通过，Python257项中发布范围检查报三个fixture无界观测队列；仅把input restriction、cursor send、direct arbitration测试队列改为容量1/try_send，原时序/去重/取消断言和生产代码不变，发布扫描规则不放宽。定向14项Rust与7项发布范围Python通过；首轮failed报告保留，修复提交后从新固定源完整复跑。
- [x] **relay适用门禁完整接入（`fix(DL2-041): include relay resource and lifecycle integration gates`）：** relay-localhost从仅session扩为该包lib/tests，覆盖已合入的resource_bounds/read_deadlines/address_family/graceful_close/process_shutdown等Windows可执行用例；实际64通过0失败3忽略，容器/显式大负载仍忽略。软件v2源bd5e166的25检查已全过；入口更新后将从新固定源统一复跑，历史结果不冒充新源。离线CPU基线源码已合入，入口scope同步为无capture/input/GPU的转换采样，不宣称完整性能验收。

- [ ] 提供 Windows/macOS 可运行的统一验证命令，以及分层可选择的重型验收。
- **证据 / 类型：工程缺口。** `scripts/verify.sh` 覆盖 Rust、Windows 前端和部分 macOS，却未汇总 account、diagnostics、Python；原 TODO 的“FFI 已连接取消 / shutdown 背压”仍未关闭。246 项前端及 Rust 输入测试均通过，但 DL2-001 的不一致依然存在。
- **方案：** fast：format/typecheck/领域与契约测试；integration：本地 transport、慢发送/慢盘、取消/代数、跨 ABI；native：捕获/编码/权限/设备；performance：可重复基准与 soak。固定 Bun 1.3.14 与仓库 Rust toolchain，输出版本、源码 SHA、命令、跳过原因。
- **新增关键覆盖：** TS→Rust 滚轮边界；FFI→Swift 可恢复错误；AppKit 实际键盘/输入法接线；shell 更新不销毁会话；backpressure 下关停；静态关键帧；拖放坐标；拓扑变化；并发注册与依赖失效 readiness。
- **验收：** 从干净依赖环境可重复运行；缺少 Apple/设备条件时明确 skipped，不能算 pass；不以源码正则匹配代替关键用户流程。只为已稳定且有明确验收标准的界面增加 DOM/交互测试。
- [x] **统一入口代码与 Windows 自动验证切片（`feat(DL2-041): add layered local verification with source-bound reports`）：** `python scripts/verify.py` 统一 fast / integration / native / perf，PowerShell/POSIX 薄入口转发；固定 Bun 1.3.14、仓库 Rust toolchain 与 frozen/locked 安装，Windows Cargo 经原生环境 wrapper；任务可单选且自动带前置，记录源码 SHA/dirty、工具版本、命令、耗时和原始日志，零测试不能误通过，超时清理整个子进程树，清理失败阻断后续任务。说明见 `docs/local-verification.md`。
- [x] **入口回归：** Python 全套 127 项通过（新增入口回归 25 项，含真实子进程/子进程树、固定版本、零测试、skipped、失败阻断与间接 Cargo 依赖）；三个 Bun 项目的类型检查和测试通过（246 / 20 / 14 项），前端生产构建、Rust fmt/Clippy/workspace lib、protocol/video/session/crypto、localhost transport/relay、FFI runtime、媒体恢复和 Windows runtime 契约均通过。实际命令与结果另存 `dist/verify/verified-local.json`，运行时源码为基线 `a905b88` 加本切片，dirty=true；不是跨分支集成基线。
- [x] **跳过语义：** `python scripts/verify.py native perf --report dist/verify/verified-device-skips.json` 实际记录 Apple/交互设备缺条件与 DL2-014 未集成，`passed=false` / incomplete；固定错误 Bun 与零匹配测试分别验证为 failed。
- [ ] **仍需验收：** macOS/Xcode 实际运行、原生权限/硬件/双机/长稳与 DL2-014 同条件性能对照；离线媒体基线入口待媒体分支独立提交 `254f8be` 集成后执行。跨层新增关键覆盖仍随对应 DL2 修复补齐，主项保持未完成，不以本地入口成功代替用户流程验收。
- [x] **ABI 检查入口增量（`feat(DL2-041): register read-only ABI header verification`）：** 检查 Apple 已提交的 `5691aba` 后注册跨平台 `--task ffi-header-sync`，只调用生成器 `--check`，不自动改头；源码未集成时 skipped。新增两项合成回归验证未集成不执行、同步检查不改头、漂移失败；统一入口 Python 全套 134 项通过（入口回归合计 27 项），证据 `dist/verify/abi-gate-python.json`。真实本基线 `--task ffi-header-sync` 记录 incomplete / passed=false（`abi-gate-unintegrated.json`），实际 ABI layout / Swift 仍待集成后验证，未复制其他分支 WIP。
- [x] **跨层门禁与 probe 接入（`feat(DL2-041): gate probe integration and pending input context acceptance`）：** 新增 `--task direct-probe` 执行 crypto/transport `--lib --tests --locked`；按授权只复用协议13契约9979、shared-probe3277和独立lane0836，精确来源→本分支SHA见能力矩阵。实际统一入口在源码 `4c3648a` 加本切片（dirty=true）通过 crypto/transport79、workspace lib259、契约109、relay27、FFI39、媒体恢复2、Windows契约131、Python134与fmt；另一报告的前端frozen安装/生产构建、全workspace Clippy通过。原始证据 `dist/verify/probe-integration.json` / `probe-clippy.json`；计数有重叠，不推导独立覆盖总量或双机性能。
- [ ] **严格 epoch 门禁的历史缺口与候选验收：** 工程5501aad的非零context/字段顺序/InputContext/保留前缀合同codec47项通过，56f53b4显式API桥接及加密loopback5项通过；其时七处旧生产调用、host映射与两平台呈现尚未接齐。当前候选保留 J 的非零 Windows producer/host 与 A 的完整接线，不沿用旧零值结论；仍须联合验证旧事件不重贴、旧release不影响新epoch、首帧实际呈现前禁用、host原子清键/推进/映射锁与取消独立生效，并补 Apple 旧 input callback receipt 后继修复。Windows合成/实际WebView和Apple presented真机分别记录；013整体验收仍 `[ ]`，不能用seq/wallclock/config_version或字段正则替代，不以编译或API原语通过标绿。
- [x] **编译/codec/运行分层入口（`feat(DL2-041): retain failed runtime gates during input context transition`）：** 新增rust-check、protocol-wire、host-runtime-contract与controller-loopback；本工程实际编译通过、protocol47/probe79通过，但HostVideoPipeline runtime9通过1失败（InvalidVideoConfig），controller加密loopback3通过2失败（旧send_input返回InvalidInput），两份整体报告均failed/passed=false；未跳过或改默认值。入口/脚本Python134项另通过。清点七处旧send_input生产直接调用，覆盖普通键鼠/Text/IME/粘贴/FFI优先及延迟release；完整9ad未pick，待产品独立Rust桥接，完整C/Swift/FFI保持Apple源所有权。实际命令与源码身份见 `input-context-transition.json`、`controller-context-transition.json`、`transition-runner-python.json`；旧probe绿色报告不替代当前业务失败。
- [x] **最小桥接实际复跑（`docs(DL2-041): record bridge loopback success without closing host gates`）：** b473498复用为56f53b4，与9ad的controller/test hunk等价、去重patch-id已记录；无C/Swift/worker祖先依赖。干净源码56f53b4的 `captured-context-bridge.json`：编译、protocol47、controller-loopback5通过0忽略；host仍9通过1失败，整体failed/passed=false。干净源码31e9fb0的fmt与Python134项通过（`captured-context-bridge-tooling.json`）。七处旧生产调用保持清单，API与测试成功不等于生产迁移；完整Apple整合保留一份等价API/test，不覆盖其他传输改动。
- [x] **有序Sender与扩展门禁（`docs(DL2-041): distinguish sender regressions from epoch integration gates`）：** 来源3ea72dc已复用d6f153e；本地干净源码报告fmt/编译/protocol47与独立loopback6通过（32并发clone共36帧连续seq、原context、零ctx不发/不耗seq），完整FFI旧Host8通过2失败、Windows host9通过1失败，整体failed（ordered-input-sender.json），不只报lib12/loopback绿。补持久stream+新Noise/重启/同进程fresh RNG、零/失败/overflow、仅映射边界推进、u64字符串/BigInt、C/Swift边界、同尺寸/原点变化配对config_version与旧buffered帧门禁。保留既有InconsistentVideoConfig拒绝，未把设计风险写成现态复现；这些新规则待owner接线，不关闭013。
- [x] **固定Windows joint来源与报告已取得（`docs(DL2-041): require joint revision evidence before closing Windows gates`）：** J5782341e22ba55b2efe30a8eeaeafbebfe0daaba/tree59c7270已核对parents27fec+65c、27/37历史、477源码blob及六份日志；Rust634通过2失败19忽略/Cargo101，Bun474/构建与八包严格Clippy通过。两FFI Host失败为完整Apple缺项，不写Rust全绿。本工程只读核对而未复跑J，完整第一波仍待下项，不操作root未知dirty。
- [x] **joint初始化保留局部回归已记录（`docs(DL2-041): gate joint completion on resumed-partial initialization safety`）：** 65c源修复/28项报告后，J原日志588/589/597/598明确记录真实初始化取消保留、空part/显式cancel、初始化错误/sync失败四项ok；本工程只读核对，不冒充自行实跑。该子项已有证据；真实慢盘预算和第一波聚合继续未完成，旧89缺陷及174/1更正计数保留历史身份。
- [ ] **新parents与完整Apple依赖联合复跑（`docs(DL2-041): bind final joint gates to updated parents and Apple dependencies`）：** 初期27fec/65c及Apple71/186来源报告保留历史身份；当前J578完整634/2/19与A f954来源165原生/201Python已绑定固定SHA，J两Host失败不能外推Apple源。第一波候选去重既有桥接/Sender并保留完整Apple历史、真实Windows非零producer，合入稳定服务/GUI/网络与Apple receipt后继修复，再实际跨包复跑；当前merge不等于测试完成，不单独flat-pick9ad，不操作root未知WIP。
- [ ] **冻结媒体不替代联合门禁（`docs(DL2-041): keep joint acceptance pending after media source freeze`）：** 65c的final-validation/handoff及三组原日志448通过0失败18忽略相互一致，source前后/锁hash/37笔提交清单已只读核对，限定包Clippy等范围单列，本工程未复跑来源。020/046/050源主项完成，其他切片或部分实现与pending保持；015 GPU surface链未实现、030仍256MiB/无目录等不能按总数关闭。待最终joint树、Apple完整稳定来源和实际跨包/适用测试聚合，再收口041/013与可交付性，18忽略不算通过。
- [ ] **第一波独立候选实际合并/复跑（`docs(DL2-041): start the protocol-13 candidate without waiting for later feature waves`）：** 独立wave1 worktree已从J578真实merge完整Apple f954历史；N4b、工程040/041/042、服务token补修后的最终来源及GUI506最终历史按稳定提交聚合，保留Windows非零producer与原问题历史。Apple新发现Swift input receipt P2由owner后继补丁追加；候选实际报告、真实事件签名接收/store闭环与未运行平台清单待交付。GPU、文件协议14、024后续开发不阻塞本轮，main由主会话在候选通过后决定合入。
- [ ] **044/045完整u64云诊断合同补修：** 合法大于2^53的stream ID仍可由native producer以JSON u64 number产出，而现服务safeInteger拒整batch；原u64_overflow fixture仅两端一致不能证明全值域正确。producer/services各自后继切片改规范有界十进制字符串、native仍u64、白名单/原因不放宽，必要计数无损；实际最终client→签名schema/store全值域闭环待候选执行，当前不勾完成。
- [x] **当前第一波软件收口（2026-10-26，干净测试源a4c5b40）：** `dist/verify/wave1/final-software-v3.json` 的25个检查全部passed，包含安装/构建/静态检查，不称25个测试。Bun前端474、账号79、诊断129，Python257，FFI完整91/0忽略，Windows完整292/17忽略，relay完整lib/tests 64/3忽略，Windows UI141/1忽略；另有workspace lib511/3忽略及all-targets/all-features check、严格Clippy、fmt通过。范围有重叠，忽略不算通过，不相加为去重覆盖率。FFI包含真实MSVC C布局，双头检查通过；relay不再只跑session。完整来源/命令见[第一波集成记录](docs/wave1-integration.md)。
- [x] **完整来源与013代码/Rust本地收口：** J578、A f954、Apple receipt/admission后继cb751、N4b、E6b、服务最终d2d4、GUI506及原生u64 producer28c11均为a4祖先，完整历史保留。去重桥接/Sender后保留Windows非零随机epoch、捕获前context/异步输出归属、完整Apple ABI/owned future/media_generation和040策略owner；当前FFI91通过，不沿用旧工程/旧joint缺完整Apple时的Host零epoch失败作为现状，也不改写其原始失败报告。013代码与Rust本地子项 `[x]`，6项Swift Recording、Swift/Xcode及实际同stream拓扑/呈现仍 `[ ]`。
- [x] **044/045全u64本地生产链收口：** 在干净a4重新运行production native签名fixture，以合成身份/临时日志产生原始body/hash/Ed25519签名，再经127.0.0.1真实HTTP接收、生产schema/store、临时文件SQLite关闭重开、去重与analysis验证，19案例全部通过、differences为空。覆盖12项当前/历史规范化正例、2项旧安全数值、4项旧不安全Number拒绝且零存储、1项重hash但保留旧签名的篡改拒绝；规范十进制string与原始签名字节保留。该结果关闭本地client签名/store/analysis切片，设备GUI与远端生产送达继续 `[ ]`；未读用户日志或执行远端上传。
- [ ] **平台/设备门禁继续待验：** `platform-conditions.json` 为incomplete / passed=false，2通过/7跳过；通过的是双头检查与合成CPU离线基线，不能称真实捕获/编码、GPU或完整性能通过。Swift/Xcode、6项Swift Recording、实际WebView/拓扑/权限、真实输入/双机、设备恢复、长稳与签名安装/发布继续待验。041软件收口子项 `[x]`，主项保持 `[ ]`；GPU235及其owner来源290/17不属于本第一波候选。
- **历史来源状态：** 本节此前5501/56f/d6/旧joint的failed报告、a97f2d4+dirty MSVC局部验证、首轮夹具失败、v2与待聚合/待u64补修记录均按其源码时点保留；本次a4最终报告收口对应软件缺项，不把历史失败改成通过，不把跳过平台项改成已验。
- [x] **第二波独立候选软件/GPU收口（干净源97ff7c0）：** 第一波冻结c3与GPU235完整历史正常merge，实测source `97ff7c08199cdad4511b0044ddf783d79e2295b1` / tree `2319d0e73bb68c22496d43ec1fb99f84f4e1dd6f` 的25软件检查全pass：Windows297/32默认忽略、workspace lib516/18忽略、FFI91/0、relay64/3、Bun474/79/129、Python257。另6条GPU选中命令8/6/4/1/1/4共24通过/0忽略，其中15项原ignored（含inventory）实际执行；5项lib默认用例与软件套件重叠，另4项example，不能跨范围求和或把默认ignored计pass。新native签名→本地HTTP/SQLite19案例也全部通过，属于单列本地生产链证据。命令/原始报告见[第二波集成记录](docs/wave2-gpu-integration.md)。
- [ ] **第二波平台/性能剩余门禁：** platform仍2通过/7跳过、incomplete / passed=false；本次选中GPU正确性用合成纹理/Recording输入，未采集真实桌面或注入真实输入。真capture、driver/device恢复、Swift/实际拓扑、设备GUI/双机、同画质性能/端到端/功耗/长稳及生产送达仍未验，041主项不关闭。第一波V/a4/c3及owner290/17保留原来源和结果，不以97ff新结果回写历史。

### DL2-042 · P2 · 统一当前能力、版本与阶段记录

- [x] 更新产品能力矩阵和当前架构契约，明确用户确认、源码能力与本次验证的边界。
- **证据 / 类型：文档缺陷。** `docs/current-architecture-contract.md:10,22,74` 仍以 0.1.91 为当前候选；package/README 已为 0.1.93。原 TODO 把 macOS VideoToolbox 当未来项，但 `apps/macos/Sources/DeskLinkApp/Capture/MacH264Encoder.swift` 已有实现；原“下一阶段”还停留在 Windows 首发冻结。
- **方案：** 以 Windows→Windows、macOS→Windows 为核心矩阵，逐项列输入/IME/音频/剪贴板/文件/多屏/权限/恢复；被控 Mac 和 iOS 单列。版本由 manifest/配置读取，历史发布验收与新阶段开发任务分离。
- **验收：** README、PRODUCT、DESIGN、架构契约及验证脚本的能力描述一致；用户确认的正常控制不再被标为未实现，尚未接通的 Mac 功能也不能因 Rust 方法存在而打勾；旧检查记录保留日期和原始范围。
- **体验约束：** 维持连接/共享两主入口、原生平台习惯和清楚的下一步；本次不把既有视觉取舍、没有装饰或旧阶段未签名一概判成远控功能缺陷。
- **完成（`docs(DL2-042): align current capabilities and manifest version evidence`）：** 新增 `docs/current-capabilities.md`，以 main `a905b88` / 工程切片 `44a2a99` 为基线，逐方向列基础控制、IME、音频、剪贴板、文件、多屏、权限和恢复；同步 README、PRODUCT、DESIGN、架构契约，保留历史记录日期。明确用户已确认两条基础远控、Mac VideoToolbox 已实现、未接通 Apple UI 不能因底层方法存在而勾选；其他分支只列固定提交证据，035 三笔链和 009 协议 13 契约不冒充已合入 main/完整反馈功能。
- **实际验证：** 只读 `python scripts/verify-windows-release.py --versions-only` 输出 manifest 的 0.1.93 / 协议 12 / Bun 1.3.14 / Rust 1.97.1；5 项新增元数据/版本漂移回归通过，空 PATH 的真实 CLI 不调用构建/原生准备，文档版本表与 README 动态对照 manifest。统一入口 `--task python-tests --report dist/verify/dl2-042-python.json` 全套 132 项通过；6 份相关文档的本地链接全部可解析，`git diff --check` 通过。源码验证为 `44a2a99` 加本切片（dirty=true）；本次未新增真机、发布或性能结论。
- **probe接入时的版本记录（`docs(DL2-042): distinguish probe integration protocol from main baseline`）：** 按后续授权复用协议13、shared-probe与DirectProbe独立提交后，工程源码快照为 `673ec15a50afdffa06f9d65462a568ad2bb8f440`，main `a905b88` 仍协议12；同步矩阵/README/架构契约和三笔来源→本分支SHA，明确不是其余功能已集成。只读版本入口实际输出13，5项版本/文档回归通过；当时013 epoch合同未提交。
- **合同过渡状态（`docs(DL2-042): label zero-context integration as non-deliverable`）：** 记录bc6c0a→本分支5501aad来源，wire/codec已落地但零生产者未接线；编译/protocol47/probe79通过而host runtime9通过1失败，整体failed/passed=false。同步矩阵/README/架构契约，禁止用此前4c3648a的绿色probe报告覆盖新源码失败；main仍协议12与用户已确认的基础路径分开。版本/文档5项回归与链接/diff检查通过。计数已按本地原始报告更正（`docs(DL2-042): correct transition verification to local test counts`），49是来源分支报告，不是本次执行数。
- **最小API桥接状态（`docs(DL2-042): record the verified runtime bridge and remaining host failure`）：** 来源b473498已复用为56f53b4，仅ControllerRuntime显式API与加密loopback；两文件hunk与完整9ad的稳定patch-id相同，后续Apple整合去重，不重复覆盖。干净源码56f53b4的本地报告：编译、protocol47、controller-loopback5通过0忽略，host runtime9通过1失败，整体failed。七处旧生产调用、host epoch、完整C/Swift/FFI和呈现仍未接齐，013不关闭。矩阵/边界合同同步，来源/patch-id/报告/链接检查通过。
- **新增审查证据分类（`docs(DL2-042): record disputed review evidence without endorsing root draft`）：** `docs/second-stage-review-evidence.md` 记录主目录156增/16删稿非工程主会话或两个子agent所写，作者未确认，保持未代stage/覆盖。只读核对045原实现已continue Some(Err)、046取消树已由3277修复而仲裁待验、048未直接入Ed签名不构成已确认P1（Noise payload先认证解密），列防御纵深与篡改/拼接验证范围。不沿用“新增全部确认”，其余新项待owner核实后再更新最终矩阵；未改crypto或新增条目状态。
- **有序Sender证据（`docs(DL2-042): separate sender success from failed host integration`）：** 来源3ea72dc→本工程d6f153e仅bc6+b473前置，保留单一Runtime/Sender实现；干净源码本地报告fmt/编译/protocol47/独立loopback6通过，完整FFI含lib12等选中项通过但旧Host8通过2失败，Windows host9通过1失败，整体failed。32并发clone/原context/连续seq与零ctx不耗seq是发送原语证据，不是生产迁移/新Noise随机epoch/IPC完整u64或呈现验收；来源与具体报告在能力矩阵分开记录。

- **044反证与逐条归因（`docs(DL2-042): correct cursor congestion evidence and observer limits`）：** 按Cargo.lock核对quinn0.11.11/proto0.11.16缓存与版本化API：send_datagram拥塞可淘汰旧包并Ok，公开错误无QueueFull，wait内部Blocked为Pending；原拥塞P1路径不成立。保留cursor真实错误?传播/可选通道分类降级的待验范围，observer只计尝试/返回错误不是真实丢包。能力矩阵逐条区分044/045反证、046取消树已修与仲裁待验、048P1证据不足；不延续全部确认。主目录草稿仍非本组写入、作者未知、保持未stage/覆盖；未改crypto/transport或其他编号状态。

- **joint初始化保留缺陷与日志归因（`docs(DL2-042): record the joint partial-retention blocker and review limits`）：** 只读固定媒体89bd1e5确认021 create/open后ready发送失败直接return，绕过正常preserve并由remove_on_drop=true删除已有resume partial；确定源码链路不是已运行回归。媒体owner独立修复与确定性回归待joint复用，未改其WIP或021状态。协调方报告joint未提交/UU/测试未跟踪，不当stable；shared174通过1忽略为其日志更正，274笔误撤回，不冒充本工程测试或joint最终验收，其余review未见问题也不等于测试通过。

- **来源更新与MacHost归因（`docs(DL2-042): scope source reports and refresh joint parent provenance`）：** 只读Git核对产品27fec756的parent16594c4与唯一InputSendFuture类型别名变更；其135通过/1签名资产忽略和本包--no-deps Clippy为来源报告，未冒充工程或跨包实跑。协调方报告Apple完整FFI71/Python186/双头通过、013 worker owned future仍在做，测试完整SHA待交付；旧工程d6/旧joint的MacHost失败因缺完整Apple链，不能外推Apple当前分支失败。最终joint第一parent更新为27fec，另一parent后续已固定媒体65c（见下条），10份resolution/测试备份及SHA256清单为协调方报告；待完整Apple依赖/稳定SHA一起合并、去重既有桥接，禁止单独flat-pick9ad，最终跨包严格检查及适用测试仍待实际报告。

- **021来源修复已交付（`docs(DL2-042): distinguish verified media source repair from pending joint acceptance`）：** 只读核对65c70fd658d0526a8a27b75fdbf120ae065222bf直接parent6dbe53d，仅TODO+transfer；ready发送失败同磁盘线程先preserve再退出。源TODO与协调方报告transfer28、lib/tests严格Clippy通过，回归源码覆盖真实open barrier/drop future、9次prefix/offset/hash保留、完整续传及空part/显式cancel/初始化错误/sync失败。当前为“源分支修复已验证、joint待复验”，不再把65c写成未修代码；协调方报告产品已开始27fec+65c真实remerge，本工程未操作joint或复跑来源，最终jointSHA/严格检查及所有适用测试报告仍待交付。

- **媒体冻结交接范围（`docs(DL2-042): record frozen media evidence without widening completion claims`）：** 只读65c的final-validation与windows-media-handoff，记录source前后SHA/空working_status/Cargo.lock和报告SHA256、448通过0失败18忽略及37笔提交；只作为来源结果，本工程未复跑。主项仅020/046/050完成，其他按切片/部分实现与pending保留；015仍CPU读回且GPU surface链未实现，030仍256MiB/无目录、大文件与持久块证明。046后续源切片不冒充本工程已有初始取消树。041/013和可交付性不因来源计数关闭，最终joint/Apple完整稳定聚合证据待交付。
- [x] **固定joint与逐项快照更新（`docs(DL2-042): bind joint results and phased ownership to immutable snapshots`）：** J578的parents/tree、27+37历史、六份日志哈希、477源码blob及原Rust634/2/19、Bun474/Clippy范围已核对，021四项初始化与013两项真实加密联合用例见原日志；不写Rust全绿。前置检查框已逐项列代码/自动验证与剩余验收，明确未合main、root044–061作者未知且未覆盖；固定A f954完整13历史、N4b第一波与S/GUI待稳定来源分开，G015和030/协议14、024后续不阻塞第一波。GUI044固定67cd不改cloud过滤，服务044/045schema进入候选后仍须真实签名接收/store闭环。
- [x] **当前第一波来源与逐项状态收口（2026-10-26）：** 首部增加V=`a4c5b404d43794d646394923a481f23c9298c3ed`、测试时干净tree154d51e，区分25检查全pass、本地签名→HTTP/SQLite19案例全过与native 2通过/7跳过incomplete；同步A+CB、服务最终d2d4和D+producer28完整merge，013代码/Rust本地、044/045全u64本地链、041软件子项完成，Swift/真实拓扑/设备GUI/远端送达/平台与性能仍待验。GPU235的290/17只列owner来源，未计入本候选；main仍a905b88/协议12，候选0.1.93/协议13尚未合main/推送/发布。证据索引为[第一波集成记录](docs/wave1-integration.md)，最终文档提交与已测试a4源码由交接manifest分别绑定。
- **本次文档验证：** 根会话实际运行current_source_versions 5项通过；文档链接/diff及最终提交由根会话统一收口。
- **历史来源状态：** 本节旧工程、媒体、产品、Apple及joint报告保留原SHA、原计数和当时失败/待验含义；上文“尚待完整Apple、服务最终来源或u64后继”均为当时快照，当前状态以首部V及本条为准，不用来源通过替代本候选实跑，也不把本地软件结果外推成设备或生产验收。
- [x] **第二波来源/进度同步（2026-10-26）：** 首部新增G实测97ff/tree2319，保留c3第一波冻结与GPU235七笔两个parents，015/016代码/软件子项改为当前独立候选实际通过；25软件、GPU选中24/0、本地签名HTTP/SQLite19与platform2/7 incomplete分别列范围，明确默认ignored/重复用例不求和。第一波V/a4及既往owner来源记录不改写，真实捕获/恢复/同画质性能和设备平台保持待验。证据索引为[第二波GPU集成记录](docs/wave2-gpu-integration.md)，后续文档提交与已测试97ff源码分别绑定；未宣称已合main/推送/部署或发布。

## 8. 扩展角色

### DL2-043 · P3 · 被控 Mac 的能力协商与实际编码一致

- [ ] 在扩展 Windows→Mac / Mac→Mac 前，统一声明能力和真正的捕获编码参数。
  - [x] 安全切片：捕获默认上限统一为 FFI 已声明的 1920×1080，VideoToolbox 请求 Main；启动拒绝超限尺寸，输出在发布配置/帧前校验 SPS profile_idc=77，且 VideoToolbox 实际格式尺寸必须等于当前配置尺寸；后端意外产出 High 或尺寸不符时明确失败而不发送未声明码流。
  - [x] 自动验证：`python -m unittest discover -s scripts/tests -p test_dl2_apple_media_contract.py -v` 2 项跨 Swift/FFI 静态合同通过；新增 Swift Main 输出、High 拒绝及捕获尺寸用例，但本机无 Swift/Xcode，未执行 Swift。
  - [ ] 仍需接通控制端 profile/尺寸约束到实际后端的动态协商，验证 High 支持端、Main-only 端、编码器降级、切屏/分辨率变化及真实 Mac；当前安全切片未宣称完整协商或硬件验收完成。
  - 提交主题：`fix(DL2-043): constrain Mac capture and encoding to declared Main capability`；验证记录及换行归一在后续 `fix(DL2-043): record verified safety slice and normalize sources` 提交。
- **证据 / 类型：源码契约缺陷。** `crates/desklink-ffi/src/host_worker.rs:741-748` 声明 Main/1920×1080；`apps/macos/Sources/DeskLinkApp/Capture/MacH264Encoder.swift:355-362` 实际配置 High，`apps/macos/Sources/DeskLinkApp/Capture/ScreenCaptureSource.swift:87-105` 默认捕获上限为 2560×1440。
- **方案：** 根据实际 VideoToolbox 后端能力构建协商结果，确保每次重配的 profile、尺寸和配置版本一致；不向对端发送其没有声明支持的码流。
- **验收：** Main-only 控制端、High 支持端、编码器失败降级、切屏/分辨率变化分别验证；真实 Mac 系统权限、签名/公证和无人值守边界另记。此项不阻塞已经跑通的 Mac→Windows。
- **iOS 关系：** AppleCore 共享修复需要回归 iOS；iOS 新功能和独立发布不自动进入本阶段关键路径。

### DL2-059 · P2 · Mac Host 把未知事件当致命错误，Transfer 流量会拆掉主机会话

- [ ] 未知事件 kind 显式忽略并计数，错误只能由显式 error 事件表达。
- **证据 / 类型：源码缺陷（ABI 漂移的运行时后果）。** `apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift:592` `hostCallbackKind` 的 `default: .error`；而 Rust 侧会发出两份手工头文件都没有的 kind：`crates/desklink-ffi/src/host_worker.rs:847` 收到控制器 Transfer 数据报时发出非终态 `HostEvent::Transfer`，`crates/desklink-ffi/src/lib.rs:416` 映射为 `DesklinkHostEventKind::Transfer`。HostBridge 把 error 转为 publishError（`332-333`），随后 `513-518` 置 `.failed` 并 stop()。
- **用户影响：** Windows 控制端向 Mac 主机发送剪贴板/文件消息（SecureLane::Transfer）时，整个 macOS 主机会话被误判为错误并拆除。
- **方案：** 依赖 DL2-025 的统一 ABI 契约：Swift 对未知 kind 记诊断并忽略；不得用 default 兜底成错误。
- **验收：** 用超出当前头文件的 kind 注入事件，Mac 会话保持；Transfer 消息进入正常确认流程而不是 failed。

### DL2-060 · P2 · FFI Host 输入新鲜度用本机墙钟，时钟偏差直接终态关闭

- [ ] 建立会话级时钟偏移，或把超窗输入降级为丢弃并告警。
- **证据 / 类型：源码缺陷。** `crates/desklink-ffi/src/host_worker.rs:840` 用 `decode_input(&plaintext, now_micros())` 做墙钟校验（5 秒 age / 1 秒 future skew），而 `crates/desklink-protocol/src/codec.rs:287-299` 专门提供了不做墙钟比较的 `decode_session_input`，注释明确说明两台机器的时钟不保证同步。控制端时间戳来自其自身 `now_micros()`（`crates/desklink-ffi/src/controller.rs:315`）。超窗时第一条输入即 `TimestampOutsideWindow` → `HostError::Protocol`，`is_retryable_host_error`（`host_worker.rs:517-519`）只放行 Transport 类错误，会话进入终态 Closed 且不重连。
- **用户影响：** 控制端机器时钟偏差超出窗口（新装机、虚拟机、双系统切换后常见）时，连接成功但第一次键鼠操作就断线，且表现为不可重试的协议错误。
- **方案：** 握手互换时间戳建立 per-session 偏移，用 `decode_session_input` 加偏移校验；或降级为丢弃单条输入并发可恢复告警。Windows 原生 host 侧一并核对是否存在同类用法。
- **验收：** 人为偏移控制端时钟 ±10 分钟，会话保持且输入可用（或明确提示校时）；不出现终态协议错误。

### DL2-061 · P3 · FFI 控制端直连静默超时被 relay 事件反复重置

- [ ] 静默判定基于"上次 direct 数据报到达时间"，而不是每次轮询新建计时器。
- **证据 / 类型：源码缺陷。** `crates/desklink-ffi/src/controller.rs:758-778` 每次进入 `next_transport_event` 都新建 5 秒 sleep；select 的 relay 分支返回任意事件（包括 host 持续经 relay 发送的光标数据报，`host_worker.rs:963-969`）后回到循环重建计时器——direct 路径被防火墙黑洞时（正是该超时要防的场景）静默判定可能永不触发，控制端停留在黑屏 direct 路径不回退。Windows 原生控制端的第一阶段实现按"5 秒静默回落并记录 datagramSilence"正确处理，本项仅涉及 FFI/Apple 路径。
- **方案：** 记录 last_direct_datagram 时刻并在循环中比较 elapsed；relay 事件不重置该计时。
- **验收：** 注入"direct 黑洞 + relay 光标持续到达"场景，5 秒内回退 relay；正常 direct 流量不误判。

## 9. 建议执行顺序与依赖

1. **立即执行 P0 服务端加固：** DL2-052 是配置级修复，先于一切其他工作；DL2-053 紧随其后。两项均为服务端独立改动，不与客户端分支冲突。
2. **先固定合同和复现用例：** DL2-014、025、041 建立基线；并行修 001、003、034～037、047、051、054～057。补测试应直接针对上述可复现/可推导分支。
3. **消除操作与恢复断点：** 002、004～013、027～029、032、044～046、048。优先证明正常控制不会因菜单、重试、输入法、静态桌面、光标拥塞、直连竞争或取消而失效。
4. **推进媒体性能：** 015/016 联合重构 Windows 捕获编码；018 优化 Apple 输入队列；017、019～022 分别优化音频、中继、文件和恢复。每项带同条件测量。
5. **补齐完整工作流：** 026、030/031、033、038/039、049/050、058；再以 023/024 推进高分辨率/高帧率和完整直连能力。
6. **同步收拢代码与文档：** 040 随纵向修复落地，042 随能力变化更新；043、059～061 作为扩展角色独立验收，其中 059、060 必须在开放 Windows → Mac 之前完成。

不要求按模块全部修完后才集成。每个纵向切片交付一条真实用户路径，避免出现“后端 API 已有，但 UI/ABI 未接通”的新半成品。

### 建议的性能验收目标

以下为第二阶段的初始工程目标，需要 DL2-014 在指定硬件/网络下校准，**不是当前实测值或对所有机器的承诺**。固定同画面、同画质、同路径后比较。

| 场景 | 初始目标 / 验收约束 | 测量方法 |
| --- | --- | --- |
| LAN 输入→画面反馈 | 参考 p95 ≤50ms；单独记录注入与视频各段 | 回显程序或外部高帧率录像；不能用 RTT 代替总延迟 |
| 1440p60 能力机 | p95 处理不积累超过一个帧周期；落后则降载 | 阶段单调计时与有界队列指标，记录实际显示而非仅编码 FPS |
| 静态画面恢复 | 关键帧请求→重新显示 ≤1 秒 | 主动重置解码器/丢关键帧后保持桌面静止 |
| 取消与退出 | 常规 UI 反馈 ≤100ms；完整停止建议 ≤2 秒 | 已连接、对端不读、发送背压和资源释放回归 |
| 不相关菜单/Host 更新 | 解码器重建次数 0，额外关键帧请求 0 | 会话存活期间记录资源创建/销毁与请求计数 |
| 音频 | 缓冲严格有界，UI 忙后不播放历史积压 | 欠载、排队年龄、实测音频延迟与断续记录 |
| 长稳 | 至少 8 小时，内存/句柄/线程/任务数无持续增长 | 固定工作负载，区分缓存平台期与持续泄漏趋势 |
| 慢盘/弱网/候选黑洞 | 输入、音频与取消不被慢路径秒级阻塞 | 故障注入并记录 p95/p99、锁持有与队列年龄 |

### 完成记录模板

每项任务在原条目追加：

- 修复提交 / 验证源码 SHA：
- 自动测试及命令：
- 真实设备、系统、硬件、网络与操作步骤：
- 前后性能数据与测量条件：
- 尚未验证、跳过项及原因：
- 最终状态：代码完成 / 自动验证完成 / 真机完成，分别勾选。

不得为了减少本清单数量，把未验证条目直接标成已完成。

## Apple/FFI 追加复核

### DL2-051 · P2 · Mac 账号 restore 遇瞬时网络故障即删除钥匙串会话

- [ ] 区分令牌失效与网络/服务不可达，只有前者清除本地会话。
- **证据 / 类型：源码缺陷。** `apps/apple/Sources/DeskLinkAppleCore/AccountClient.swift:127-132` restore 中 me 请求失败后尝试 refresh，refresh 再失败落入外层 catch 执行 `clearLocalSession()`（删除 Keychain token 并置 signedOut）。启动时无网或账号服务瞬时不可达即被永久登出。Windows 侧 `apps/windows-ui/src-tauri/src/account.rs:80-127` 失败仅返回 Err 并保留存储会话，两端行为不一致。
- **方案：** 网络类错误保留会话进入"离线、稍后重试"；仅明确的令牌无效/被撤销才清除；与 DL2-035 的迟到响应隔离一起设计。
- **验收：** 断网启动、服务 5xx、refresh token 过期三种场景行为各自正确；恢复网络后自动或一键恢复登录。

- [x] 代码与离线入口：HTTP status + 稳定 error code 分类；仅 `/me` 的明确令牌错误触发 refresh，仅 refresh 明确 `401 / token_invalid` 清除会话。网络、服务、429、解码、取消及 Keychain 故障保留凭据并进入离线可重试状态；Mac/iOS 本机工作区继续可用。single-flight 与 session generation 隔离迟到恢复 / 刷新 / 登出；旋转令牌落盘失败先保留内存新令牌，避免重试旧 refresh。主题 `fix(DL2-051): preserve Apple account sessions across transient restore failures`。
- [x] Windows 静态验证：`python -m unittest scripts.tests.test_dl2_apple_account_restore_contract -v`，8 项协议与源码合同通过；`git diff --check` 通过。新增 Swift mock transport + Keychain spy 行为用例覆盖断网、5xx、429、明确失效、重试、并发恢复、迟到响应、取消和持久化错误，全为合成数据且不访问真实服务 / 系统 Keychain。
- [ ] Apple 自动 / 真机：Windows 无 Swift/AppKit/UIKit，新增行为用例尚未运行；断网启动、真实 Keychain 锁定 / 恢复、账号服务恢复后一键重试及明确失效后的重新登录仍需 Mac/iOS 验收，不能以源码合同代替。

- [x] 取消与 refresh 旋转跟进：只允许已完整收到的 refresh 2xx 在取消后继续验证；按 session generation 检查并先保留新令牌到内存及 Keychain，再表现取消。Keychain 失败仍保留内存新 pair 供重试，旧 generation 的响应不能覆盖新登录或复活已登出会话。`python -m unittest scripts.tests.test_dl2_apple_account_restore_contract -v` 实跑 10 项通过；新增 3 个 Swift 方法（取消成功响应含写成功 / 失败两分支），总 17 个行为测试仍未运行。主题 `fix(DL2-051): checkpoint rotated refresh tokens before honoring cancellation`。

### DL2-059 · P2 · Mac Host 把未知事件当致命错误，Transfer 流量会拆掉主机会话

- [ ] 未知事件 kind 显式忽略并计数，错误只能由显式 error 事件表达。
- **证据 / 类型：源码缺陷（ABI 漂移的运行时后果）。** `apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift:592` `hostCallbackKind` 的 `default: .error`；而 Rust 侧会发出两份手工头文件都没有的 kind：`crates/desklink-ffi/src/host_worker.rs:847` 收到控制器 Transfer 数据报时发出非终态 `HostEvent::Transfer`，`crates/desklink-ffi/src/lib.rs:416` 映射为 `DesklinkHostEventKind::Transfer`。HostBridge 把 error 转为 publishError（`332-333`），随后 `513-518` 置 `.failed` 并 stop()。
- **用户影响：** Windows 控制端向 Mac 主机发送剪贴板/文件消息（SecureLane::Transfer）时，整个 macOS 主机会话被误判为错误并拆除。
- **方案：** 依赖 DL2-025 的统一 ABI 契约：Swift 对未知 kind 记诊断并忽略；不得用 default 兜底成错误。
- **验收：** 用超出当前头文件的 kind 注入事件，Mac 会话保持；Transfer 消息进入正常确认流程而不是 failed。

- [x] 当前分支复核：致命误分类已由 `39c17ad4255e0c19f9b86a7a3d10fbcca3e7abbe` 修复并由生成 ABI 保持 Transfer=8；本次补未知 kind 固定计数，仅明确 error 才读错误文本，unknown不解析数据/metrics或拆会话。新增共享mapper的未知kind/极大无关字段/Transfer Swift回归（未执行），Windows 2项跨语言合同通过。主题 `fix(DL2-059): count unknown host events without interpreting their payload`。
- [ ] Swift/macOS事件注入验收未执行；当前Transfer仅显示不支持提示，正常文件/剪贴板确认流程属于026待补，不能将整项勾完。旧证据描述的是025前状态，本分支已无default:error。

### DL2-061 · P3 · FFI 控制端直连静默超时被 relay 事件反复重置

- [ ] 静默判定基于"上次 direct 数据报到达时间"，而不是每次轮询新建计时器。
- **证据 / 类型：源码缺陷。** `crates/desklink-ffi/src/controller.rs:758-778` 每次进入 `next_transport_event` 都新建 5 秒 sleep；select 的 relay 分支返回任意事件（包括 host 持续经 relay 发送的光标数据报，`host_worker.rs:963-969`）后回到循环重建计时器——direct 路径被防火墙黑洞时（正是该超时要防的场景）静默判定可能永不触发，控制端停留在黑屏 direct 路径不回退。Windows 原生控制端的第一阶段实现按"5 秒静默回落并记录 datagramSilence"正确处理，本项仅涉及 FFI/Apple 路径。
- **方案：** 记录 last_direct_datagram 时刻并在循环中比较 elapsed；relay 事件不重置该计时。
- **验收：** 注入"direct 黑洞 + relay 光标持续到达"场景，5 秒内回退 relay；正常 direct 流量不误判。

- [x] 持久 direct 接收时间/绝对截止；只有 direct 收包刷新，relay事件或取消轮询不刷新。真实localhost relay QUIC + Noise认证direct探测：旧实现黑洞/relaycursor红测超5.75s；修后黑洞与持续6s direct双回归均通过（生产5s预算，750ms测试调度容差），并已跑本分支FFI完整62项通过。主题 `fix(DL2-061): keep direct silence deadlines across relay events`。
- [ ] 真双机/防火墙与Mac/iOS生产direct启用验收未执行。现源码仅Platform::Windows创建direct_session；这是FFI共用启用路径证明，不能称当前Mac实机缺陷或H264显示/性能证明。

### DL2-060 · P2 · FFI Host 输入新鲜度用本机墙钟，时钟偏差直接终态关闭

- [ ] 建立会话级时钟偏移，或把超窗输入降级为丢弃并告警。
- **证据 / 类型：源码缺陷。** `crates/desklink-ffi/src/host_worker.rs:840` 用 `decode_input(&plaintext, now_micros())` 做墙钟校验（5 秒 age / 1 秒 future skew），而 `crates/desklink-protocol/src/codec.rs:287-299` 专门提供了不做墙钟比较的 `decode_session_input`，注释明确说明两台机器的时钟不保证同步。控制端时间戳来自其自身 `now_micros()`（`crates/desklink-ffi/src/controller.rs:315`）。超窗时第一条输入即 `TimestampOutsideWindow` → `HostError::Protocol`，`is_retryable_host_error`（`host_worker.rs:517-519`）只放行 Transport 类错误，会话进入终态 Closed 且不重连。
- **用户影响：** 控制端机器时钟偏差超出窗口（新装机、虚拟机、双系统切换后常见）时，连接成功但第一次键鼠操作就断线，且表现为不可重试的协议错误。
- **方案：** 握手互换时间戳建立 per-session 偏移，用 `decode_session_input` 加偏移校验；或降级为丢弃单条输入并发可恢复告警。Windows 原生 host 侧一并核对是否存在同类用法。
- **验收：** 人为偏移控制端时钟 ±10 分钟，会话保持且输入可用（或明确提示校时）；不出现终态协议错误。

- [x] FFI Host 会话输入策略：每个认证 Noise 会话独立校准 controller / host 墙钟；目标切换重新校准，纯画质重配保留锚点，严格递增序号门槛在同一会话中保持。先丢弃未发布 / 旧 context，再处理时钟及序号，避免旧目标的极值污染后继；时钟跳变重新校准最新认证输入，不再终态关闭；序号耗尽必须新建 Noise 会话。主题 `fix(DL2-060): calibrate host input clocks per authenticated session`。
- [x] Windows 实跑：4 项纯策略测试和 3 项真实 QUIC / Noise 本地回环覆盖正负 10 分钟、双端时钟跳变、重复序号、旧 context 极值、u64 边界；整套 `python scripts/run-windows-cargo.py test -p desklink-ffi --lib --tests --locked` 共 71 项通过。
- [ ] Apple 真机：Mac Host 与真实 Windows / iOS controller 的时钟偏差、系统校时及输入恢复仍需设备验收；本地回环不代表系统事件注入或硬件证明。

## MVP 后产品优化（第三阶段，2026-10-28 起）

> 背景：Windows/macOS/iOS 三端 MVP 已可用并完成合并（`23ba00d`）、固定密码放开（`a4d80d0`）。本阶段目标：连接速度、界面简洁、用户体验、代码完备、测试程度五条线的可验证改进。
> 纪律沿用本文件既有约定：完成一项并通过对应验证后立即独立 commit，同时勾选该项；每项写明验收标准。编号 MVP-xx。不考虑时间与人力成本，但每项必须有可复现的验证手段。

- [ ] **MVP-00 本清单落稿**：本节写入 TODO.md 并提交。
  验收：git 历史可见本节首次提交。

- [x] **MVP-01 文档：合并架构决策记录**。2026-10-27 大合并引入的关键决策散在提交信息里：回调事件枚举重编号（Capabilities=13，Session/Clipboard/AudioPcm/File=14..17）、双输入路径（CapturedInputContext 显式上下文 + 会话门控派生上下文）、统一重连语义（可恢复错误保 runtime；显式重连边界退休输入并要求新握手）、固定密码 BLAKE2 派生目录码、会话命令经 SessionIo 走有序发送通道。写入 `docs/apple/merge-architecture-2026-10.md`，供后续开发对照。
  验收：文档存在且内容与 `crates/desklink-ffi`、`ControllerBridge.swift` 当前实现一致（关键 file:line 抽查）。

- [x] **MVP-02 运维：单域名迁移阶段 2（已批准方案）**。中继身份收敛到 `p2p.yxswy.com`：Apple 端默认 `relayURL=quic://p2p.yxswy.com:4433`、serverName 同步；Windows `product-config.ts` 的 `MANAGED_RELAY_SERVER_NAME` 同步；部署文档与探针脚本参数更新。服务端保持双 SAN 证书过渡，旧保存记录（serverName=turn.p2p）继续可用；全部设备重新配对后再执行阶段 4（证书收缩 + 删 turn.p2p DNS），阶段 4 不在本轮。
  验收：三端测试引用新默认值通过；探针对 `p2p.yxswy.com:4433`（IP 直连验证 DNS 同值）握手成功；旧保存记录连接路径测试仍绿。

- [x] **MVP-03 连接速度：目录连接复用**。`connect_controller` 目录分支先用独立连接做 lookup、再新建连接入会话，每次新鲜目录连接多付一次完整 QUIC+TLS 握手（中继路径约 2-3 个 RTT）。改为 lookup 后在同一连接上直接 `join_relay` 入会话。
  验收：Rust 测试证明目录路径只建立一次中继连接（连接计数或 observer）；现有 host_runtime/controller_runtime 全绿；实际连接建立时间对比记录在提交信息。

- [x] **MVP-04 连接速度：初始静音移出关键路径**。`run_connected` 目前在进入事件循环前阻塞等待 `set_audio_enabled(false)`（最长 5 秒超时），视频首帧被这一 RTT 串行阻塞。改为把静音作为进入循环后的首个有序 owned send，与事件接收并行。
  验收：worker 测试断言"新控制器先静音"仍成立且顺序在任何用户音频命令之前；事件循环不再被静音 RTT 阻塞（测试：静音未完成时 VideoConfig 事件可被处理）。

- [x] **MVP-05 体验：macOS 首次使用引导**。无最近设备时连接页给三步引导（对端开共享 → 输入 12 位设备号与密码 → 点击连接），消除首次使用空窗。
  验收：SwiftUI 测试/快照断言空状态渲染引导且在出现最近设备后隐藏；macOS 测试套全绿。

- [x] **MVP-06 体验：会话终态失败一键重连**。核查 SessionView 在 `.failed` 终态是否保留原目标的一键重连入口；缺失则补（沿用连接页失败重试的原目标保留语义）。
  验收：核查结论——连接页在终态失败时已保留原目标并显示「重试本次连接」（凭证与保存主机两条路径均有）；新增 ConnectionRetryTests 覆盖 failed → 一键重试 → 回到连接流程且目标不变；macOS 187、Apple 129 全绿。

- [x] **MVP-07 完备：FFI 诊断日志**。`DesklinkConfig.log_level` 目前是死参数，现场排障（如 DNS/中继故障）无任何日志。实现 `DESKLINK_LOG` 环境变量开启的结构化 stderr 日志：连接阶段推进、重连调度（次数/延迟/原因分类）、目录查找失败码、终态错误。默认关闭零开销。
  验收（调整）：DESKLINK_LOG 未设时零输出有单元断言；四个生命周期事件（connect/reconnect 尝试、终态失败含原因、重连调度含次数/延迟/原因、worker 关闭）已接线为结构化 JSON 行；clippy/fmt 全绿。进程级 stderr 端到端捕获不做自动化。

- [x] **MVP-08 完备：修复 transfer_task 测试在 macOS 的失败**。`desklink-windows` 的 transfer_task_catalog/transfer_task_tests 共约 22 项在 macOS 失败（main 已有），按目录项语义差异（目录 kind/替换/重扫）逐项修复或按平台显式跳过并注明原因，不得静默删除。
  验收：根因为 macOS 的 /var 符号链接前缀触发目录钉住链的 reparse 拒绝（catalog 夹具与 transfer.rs 临时目录改为规范化路径），以及加密 actor 测试在默认并行度下的资源竞争（fixture 持全局串行锁，与既有 DISK_WORKER_TEST_GATE 模式一致）。`cargo test -p desklink-windows --lib` 在 macOS 连续两轮 280 项全绿；Windows 路径行为未改动。

- [ ] **MVP-09 测试：Apple/Rust macOS CI**。新增 GitHub Actions macOS workflow：cargo fmt --check、clippy -D warnings、workspace 测试（除显式 skip）、`apps/apple` 与 `apps/macos` swift test（先安装 cmake 供 opus-head-sys）。当前仓库无任何 macOS Swift CI。
  验收：workflow 在本仓库跑通一次全绿；提交状态徽记可用。

- [x] **MVP-10 测试：固定密码保存材料端到端重连**。Rust e2e：固定密码目录连接成功 → 保存材料 → 断开 → 用保存材料重连（不再输密码）成功且身份一致；覆盖派生码路径。
  验收：新 e2e 测试通过并纳入 verify-macos-runtime 门禁链。

## 10. 第一阶段历史记录

以下原文按本次审查前状态保留，仅供追溯。里面的“当前”“未验收”“下一阶段”、旧版本、线上探针结果和勾选项均属于原记录日期；第二阶段以本文前九节为准。尚未完成的签名、安装或设备组合验收可以继续引用，但不能用来否定用户本次已确认的两条远控路径。

<details>
<summary>展开第一阶段原始 TODO、发布门禁与历史证据</summary>

# DeskLink 上线 TODO（历史原文）

> 目标：先把 Windows 两台电脑的远程控制做成可发布的稳定版本，再扩展 4K 和公网 P2P。
> 当前判断（2026-08-15）：Windows 首发核心功能、自动化门禁、公网中继和云诊断均已通过；稳定候选必须以 clean `main` 以及同一批安装包 manifest、候选 ZIP、中继镜像和审计报告中的源码 SHA 为准。正式上线仍受 Authenticode 签名、发布 tag 和真实 Windows/SmartScreen 验收约束。账号服务已降为可选能力，未部署账号服务不再阻塞本机模式。
> 规则：`[x]` 只表示代码或自动验证已完成；需要两台真实 Windows 电脑的项目必须由人工验收后再勾选。

## 本地底座整理（2026-10-21）

- [x] 2026-10-23：0.1.92 接入启动检查、客户端更新提示、下载 / 取消 / 重试、独立签名校验和安装器更新交接；246 项前端、90 项桌面端、19 项安装器测试通过。实际公共版本发布和双机在线升级仍待验收，见 [客户端更新](docs/windows-client-updates.md)。
- [x] 2026-10-23：修复真实双机反馈的控制端中文输入法缺口，增加可编辑输入承载、组合提交 / 候选键处理和有序文本发送，保留现有协议；232 项前端、82 项桌面端测试通过，见 [中文输入修复记录](docs/2026-10-23-chinese-input-fix.md)。

- [x] 修复账号混合窗口限流、诊断批次原子写入、首次 join 超时与可靠流取消边界。
- [x] 修复恢复关键帧分片乱序时误清本帧数据，并以确定性分片顺序和跨帧确认屏障验证。
- [x] 修复主机审批刷新、审批焦点 / 过期禁用，以及进入账号页前的远程展示资源清理。
- [x] 移除 Windows 旧视觉主题，建立独立的布局、原生控件与交互 / 可访问样式基础。
- [x] 重新设计连接 / 共享双主入口与简洁青绿界面，统一表单、列表和设置；194 项前端测试与生产构建通过，真实双机验收仍独立进行。
- [x] 恢复已知 V1 配对记录的只读兼容，保留原 DPAPI 文件；修复共享状态订阅竞态，并恢复缺失的公网中继及独立证书续期。202 项前端测试、77 项 Tauri 测试、旧记录回归、原生连接 / 重连测试和公网探针通过，见 [连接与共享修复记录](docs/2026-10-21-connection-repair.md)。
- [x] 完成临时 / 固定密码与断线重连的本地流程复核，修复邀请重用的异常断开、密码页迟到响应与连接快照竞争；213 项前端、80 项 Tauri、8 项主机集成测试通过，重连后确认实际 H.264 出帧。真实双机操作验收仍独立进行。
- [ ] 补齐 FFI 已连接发送阶段的取消 / shutdown 背压回归与退出策略。
- [ ] 整理覆盖 Rust、三个 Bun 子项目和 Python 的本地验证入口。

本轮证据与后续拆分方向见 [底座检查记录](docs/2026-10-21-foundation-review.md)。以下线上、安装与真实设备记录保留原日期，本轮没有重新宣称其通过。

## 0. 发布基线与工作区（P1）

- [x] 整理当前工作区变更，确认 DirectLan、视频质量、UI、诊断和本机账号模式都属于本次发布范围。
- [x] 删除未发布版本仍保留的旧 host 安装入口和旧 relay join/directory 信封路径；当前构建只维护单一安装入口与当前协议线。已知 V1 本地配对记录于 2026-10-21 恢复只读兼容，不改变网络协议。
- [x] 将 Windows-only 发布边界接入 `verify-windows-release.py`，防止 README 重新宣传未纳入本版本的 macOS/mobile 产品面。
- [x] 让发布验证、安装包清单和 GitHub Release 绑定同一份源码提交 SHA，并拒绝脏工作区进入签名发布。
- [x] 让人工验收记录同时绑定候选安装器的精确文件名和 SHA-256，避免发布证据指向同哈希的非候选文件名。
- [x] Windows CI 和签名候选 artifact 自动上传与安装包绑定的人工验收模板，避免测试人员手工复制版本或 SHA。
- [x] 远程画面质量使用最近 10 秒滚动窗口，并在明显卡顿时提供一键切换流畅画质的恢复动作。
- [x] 防止连接命令快照覆盖实时状态通道，避免连接成功后界面短暂回退到旧状态。
- [x] 直连视频静默 5 秒后自动回落中继，并记录 `datagramSilence` 原因，避免直连黑屏卡死。
- [x] 将账号服务降为可选能力；未登录时主机仍启动，Windows 本机模式可直接使用，账号流程保留在“更多”菜单。
- [x] 更新版本号、变更日志和发布说明，形成当前候选基线（正式 tag 仍等待签名与人工验收）。
- [x] 在干净工作区上通过 Rust、Bun、安装包和发布校验（2026-08-13；本地候选门禁通过，Windows CI 历史基线 runs `30571054699` 与 `30572591597`）。
- [x] 将原生 Windows resilience 报告绑定源码 SHA 并纳入 readiness；捕获、编码、主机恢复、电源恢复和媒体 soak 证据不可再脱离候选物料。
- [x] 为桌面捕获增加首帧保障：DXGI 静态桌面超时后执行一次 GDI 初始快照，并在首帧 10 秒内未产生时交付可恢复的捕获错误，避免连接在线但画面永久黑屏。
- [x] 修复可靠视频配置与不可靠视频分片乱序导致的静态首帧丢失：控制端以有界队列暂存配置前分片，配置到达后回放并继续使用独立光标通道。
- [x] 为 Windows 视频管线增加首个编码帧看门狗：捕获已开始但编码器连续 10 秒不产出访问单元时交付可恢复的编码错误，避免 `NeedMoreInput` 导致假在线黑屏。
- [x] 停止/退出路径清理残留的配对状态，覆盖 worker 超时 abort 后的下一次启动回归测试。
- [x] 为普通主机 worker 的准备、停止和异常事件增加生命周期代数隔离，防止旧任务在重启后覆盖当前状态。
- [x] 将生命周期代数检查、状态发布和 worker 启动串入同一同步闸门，覆盖检查后写入和停止后启动的竞态。
- [x] 为控制端连接循环传递连接代数，并隔离迟到的旧连接状态事件，覆盖取消、切换设备和重连中的回写。
- [x] 为设备查询阶段的中继连接和目录请求增加连接代数取消闸门与 8 秒超时，避免查找设备时网络黑洞导致界面永久等待。
- [x] 为已保存设备连接阶段的中继建连增加同一连接代数取消闸门与 8 秒超时，避免跳过目录查询时重新进入无限“建立连接”。
- [x] 为 Windows 主机首次连接和重连增加 8 秒中继连接上限，网络黑洞会进入既有退避恢复，而不是长期停在“连接中”。
- [x] 为主机与控制端的 relay join 握手增加同一 8 秒超时，避免 TCP/QUIC 已建立但中继不返回 join 确认时继续假连接。
- [x] 为原生 FFI 控制端、主机端的建连、重连与目录查询补齐同等有界超时，避免非 Windows 宿主调用长期阻塞连接 worker。
- [x] 为中继同一主机身份重复启动补充替换回归测试；旧主机连接迟到断开时不会清除新主机、控制端或设备目录登记。
- [x] 传输层对带代数的陈旧 `PeerDisconnected` 增加 250 毫秒替换确认窗口；新对端首个可靠流到达时丢弃旧断开，真实断开仍在窗口后交付，避免控制端无谓重连。
- [x] 将持久连接标记写入新的 DPAPI 控制端存储格式；固定密码主机的目录暂时不可见或 relay session 重建时进入可取消的 6 段恢复窗口，临时邀请仍在失效后立即停止。
- [x] 固定密码连接在首轮退避预算耗尽后继续以最大间隔等待主机恢复；只有用户取消、认证不匹配或临时邀请失效才会停止，避免已验证的连接退回手工重试。
- [x] 将“连接设备”保留为唯一一级导航，其他管理入口统一收进“更多”菜单，并用 v-sans / 系统字体契约固定 1920/2560 宽屏层级。
- [x] 首次启动默认进入本机连接工作区，账号服务保持可选且不阻塞远程控制。
- [x] 本机模式快照读取失败时启用同等的自动恢复计划，避免首次启动停在暂时性错误页面。
- [x] 为可选账号服务请求增加 12 秒客户端超时，账号网络异常不会阻塞本机远程控制或设置页面。
- [x] 账号存储不可用时以未登录本机模式启动，不再因可选账号模块初始化失败退出整个应用。
- [x] 账号 HTTP 客户端初始化异常时保持账号能力快速失败，不回退到无界网络请求；本机模式仍可继续使用。
- [x] 正式 Release 上传前重新验证最终安装器 Authenticode 签名，防止签名清单与实际二进制脱钩。
- [x] 中继证书加载改用 `rustls::pki_types::pem::PemObject`，移除已停止维护的 `rustls-pemfile` 运行时依赖；workspace 级 RustSec 告警仍按平台依赖单独复核。
- [x] 将 RustSec 标记为线程安全问题的 `event-listener` 从 5.4.1 升级到已修复的 5.4.2；剩余 GTK/GLib 告警仅来自非 Windows 桌面依赖链，继续按平台边界跟踪。
- [x] 发布预检同时校验公网中继探针、线上容器源码 revision 与候选提交，避免旧中继通过网络探针但与候选版本不一致。
- [x] 短时公网探针显式关闭所有临时 QUIC 连接，避免监控探针把中继主机登记和会话容量占用到空闲超时。
- [x] 候选 Windows bundle 按当前提交和安装包哈希打包，防止同版本 ZIP 混入旧构建物料。
- [x] 发布预检同时校验 annotated tag 的解引用提交必须等于候选 HEAD，避免旧提交上的同名 tag 错误放行。
- [x] 人工验收记录升级为结构化 schema 2；双机、安装信任和 4 小时长稳通过时必须分别提供脱敏环境、安装和指标证据，不能只填写布尔值。
- [x] schema 2 验收记录启用字段白名单与敏感文本拒绝，阻止设备 ID、密码/密钥标签、PEM 私钥和本地 Windows 路径进入候选 ZIP。
- [x] 参考 RustDesk、MeshCentral、FreeRDP 的公开工程实践，整理当前产品不采纳项与后续 relay 高可用、安全扫描和会话诊断方向，见 [GitHub 同类项目架构对照](docs/github-architecture-benchmark.md)。
- [x] 修复静态桌面导致编码 resilience smoke 只收到首帧的问题；测试复用最近捕获帧并推进单调时间戳，运行时避免按固定帧号重复触发关键帧请求。
- [x] 接入独立安全扫描门禁：CodeQL 覆盖 Rust/TypeScript，Cargo audit 阻塞真实漏洞和 yanked 包；非 Windows Tauri 图形依赖的历史维护告警保留在日志中单独跟踪，见 [安全扫描门禁](docs/security-scanning.md)。
- [x] 为控制端连续滚轮输入增加有界合并，避免 IPC/网络阻塞时输入队列无界增长；键盘和鼠标按键仍保持严格顺序。
- [x] 将被控端单个被拒绝、过期或越界的远程输入样本隔离处理；缩放、切屏或迟到鼠标事件不会再重建主机 worker。
- [x] 将控制端和主机端的文件发送后台完成通知都改为容量为 1 的有界通道，断线期间迟到任务不会在旧会话中积累无界通知。
- [x] 将被控端捕获线程的屏幕/画质/profile 命令队列改为容量 4 的非阻塞有界通道，捕获线程卡住时沿既有恢复路径退出，不让控制循环无限积压命令。
- [x] 将 Windows 发布验证加入生产 Rust 无界 channel 静态门禁，防止后续稳定性修复重新引入不可控的事件积压。
- [x] 增加独立 Windows nightly/手动 300 秒 resilience 工作流；普通 PR 保持短反馈，长期捕获、编码、恢复和中继回归持续运行并上传脱敏证据。
- [x] 为 relay 目录查询限流表增加固定容量和最旧项淘汰，防止伪造设备 ID 的查询在单个限流窗口内消耗无界内存。
- [x] 为 relay 每条已认证连接限制可靠流转发任务为 16 个，异常连接超限时关闭，避免 Tokio 任务数量无界增长。
- [x] 为 relay 连接 admission 拒绝路径增加 64 个任务的上限；超出上限的 QUIC 握手立即拒绝，避免连接洪泛在达到容量后继续派生无界任务。
- [x] 为 Windows 主机 DirectLan 候选探测增加 2 个并发任务上限；重复候选超限时明确拒绝并继续使用中继视频，避免认证会话内的探测任务无界增长。
- [x] 为 Windows 托盘状态命令使用容量 128 的非阻塞有界队列；托盘窗口忙碌或队列暂满时丢弃提示，不阻塞主机连接、视频和恢复线程。
- [ ] 创建 `v0.1.91`（或下一版本）发布 tag，并保留可回滚提交。

## 1. 云端诊断可用性（P1）

- [x] 将 `desklink-diagnostics` 的 `/desklink-diagnostics/health` 和 `/desklink-diagnostics/v1/batches` 路由加载到生产 Nginx。
- [x] reload Nginx 后验证公网 health、限流和批量上报接口。
- [x] 运行 `scripts/audit-managed-diagnostics.py`，保存审计报告。
- [x] 在 Windows 客户端完成一次脱敏诊断上传，确认云端可检索且不包含密码、密钥、屏幕内容。

## 2. 局域网直连闭环（P1）

- [x] 让 `ControllerRuntime` 的 DirectVideoPath 状态机真正驱动候选探测和激活，而不是只根据已建立连接推断路径。
- [x] 增加认证探测、状态机激活和视频 datagram 传递的 transport loopback 测试；探测本身不会放行 4K。
- [x] 覆盖控制端对端先发起探测时的并发窗口，避免 Noise 会话锁导致假超时。
- [x] 被控端增加入站 DirectLan 探测监听，与已有出站连接互为兜底；Rust Windows 包测试、Clippy 和认证 datagram 回环回归测试已通过。
- [x] 补充 DirectLan transport 回环 E2E 夹具：成功、状态机超时、候选过期、认证绑定失败、主动关闭；HostRuntime ↔ ControllerRuntime 双机仍由 Windows 验收。
- [x] 视频 datagram 发送失败时立即撤销 DirectLan 句柄并重发同一包到中继；自动回落测试通过。
- [x] 增加 FFI 回归测试：直连候选被拒绝后视频回落中继、回落原因可观测，控制输入和关键帧请求仍保持中继；剪贴板/文件的双机行为继续由人工验收。
- [x] 增加直连 RTT、丢包和当前路径诊断字段；回落后仍保留最近探测质量和回落原因，避免用户看到“已直连”但实际仍走中继。

## 3. Windows 双机验收（P1，需人工）

- [ ] 同一局域网：两台 Windows 配对、审批、视频、鼠标、键盘、多屏切换。
- [ ] 不同网络：通过公网中继完成同样流程。
- [ ] 断网/切 Wi-Fi/中继重启后自动恢复，不出现重复会话或“主机服务已停止”。
- [ ] 睡眠唤醒、锁屏、窗口最小化/恢复、DPI 不同和双屏场景。
- [ ] 剪贴板、单文件/多文件传输、暂停/恢复、取消和失败重试。
- [ ] 连续运行至少 4 小时，记录 CPU、GPU、内存、帧率、延迟和错误日志。

## 4. 安装与信任（P1）

- [ ] 获取 Windows Authenticode 代码签名证书并安全保存 PFX。
- [ ] 配置 GitHub Actions secrets：`WINDOWS_SIGNING_PFX_BASE64`、`WINDOWS_SIGNING_PFX_PASSWORD`。
- [ ] 运行签名构建，确认安装包和主程序签名有效、证书链完整。
- [ ] 在全新 Windows 账户验证 SmartScreen、WebView2 Runtime 检查、安装、升级、卸载和数据保留策略。
- [ ] 生成带 SHA-256、签名状态和构建提交的发布清单。

## 5. 产品文档与 UI 收口（P2）

- [x] 清理 README 和历史版本说明，移除“直连尚未启用”等过时描述。
- [x] 清理安装器和 relay wire 的旧兼容分支；本地配对仅额外接受严格校验的已知 V1 格式，保存仍使用 V2。
- [x] 明确产品能力边界：当前最高 2560×1440；4K 和公网 NAT 穿透暂不承诺。
- [x] 统一 `DESIGN.md` 与 Windows UI 样式 token，删除多轮迭代产生的冲突规则。
- [ ] 手动验收键盘导航、焦点可见性、高对比度、WebView2 缩放和中文文案溢出。
- [x] 将发布前检查、服务器回滚和日志脱敏流程写入 [Windows 发布运行手册](docs/windows-release-runbook.md)。
- [x] 将同网/跨网、断线恢复、双屏/DPI、剪贴板/文件和长稳验收步骤整理到 [Windows 双机验收记录](docs/windows-two-pc-acceptance.md)。

## 6. Apple 端开发与验收（2026-07-26）

- [x] Rust FFI 增加显式 macOS/iOS controller platform，保留 macOS 默认 ABI 入口。
- [x] 抽取跨 macOS/iOS 的 `DeskLinkAppleCore`，统一 Keychain、controller bridge、H.264 解码和视频几何。
- [x] 完成 macOS Apple Silicon controller/host bundle、权限声明、验收脚本和未签名检查。
- [x] 完成 iOS 16+ controller-only 工程、device/simulator Rust xcframework、配对和保存设备。
- [x] 完成 iOS Metal 视频、直接触控/轨迹板、键盘、快捷键栏和 scene lifecycle 恢复。
- [x] 通过 Rust/Swift package/iOS device build/simulator unit+UI 自动门禁。
- [ ] 在真实 iPhone 上安装签名包，验收 Keychain、二维码、视频、触控、键盘、断线和前后台恢复。
- [ ] 完成 macOS Screen Recording/Accessibility 权限与同机双实例 controller/host 验收。
- [ ] 完成 Windows↔macOS、Windows↔iOS、macOS↔iOS 跨设备验收，以及 macOS 签名/公证。

详细记录见 [Apple 平台验收汇总](docs/apple/2026-07-26-apple-platform-acceptance.md) 和 [iOS controller 开发说明](docs/apple/ios-controller-development.md)。

## 7. 后续增强（不阻塞首个 Windows 正式版）

- [ ] 4K 原生编码能力评估、硬件矩阵和自适应码率策略。
- [ ] 公网 STUN/ICE/TURN 路径评估，决定是否增加真正的跨网 P2P 视频通道。
- [ ] 增加第二 relay 节点、故障切换演练和容量/证书续期告警；保持业务协议与端到端密文不变。
- [x] 增加独立 CodeQL 与依赖审计门禁，区分新增安全问题和历史平台依赖告警。
- [ ] UAC/安全桌面、语音输入、虚拟桌面等是否纳入下一版本范围。
- [ ] macOS VideoToolbox 适配（仅当恢复跨平台发布目标时）。

> 4K 评估已补充真实 Windows 捕获/编码探针和放行门槛，待真实设备运行并记录硬件矩阵后再勾选完成；见 [Windows 4K 编码评估](docs/windows-4k-evaluation.md)。

## 下一阶段执行顺序（发布冻结前）

1. **候选基线**：以当前提交和安装包 SHA 生成候选验收记录模板，确认发布说明只描述已验证能力，不扩展 4K 承诺。
2. **双机验收包**：使用现有 `verify-windows-resilience.py --soak-seconds 300` 和候选安装包，记录同网直连、跨网中继、断线恢复、双屏/DPI、剪贴板和文件传输结果；正式发布预检会拒绝低于 300 秒的 resilience 报告。
3. **信任与安装**：用户提供 Authenticode 证书后执行签名构建、SmartScreen 和全新账户安装升级验收；没有证书时保持候选版，不伪装成正式发布。
4. **发布冻结**：在干净 checkout 重跑所有门禁，生成 SHA-256/签名清单，最后创建 `v0.1.91` tag；4K、公网 P2P 和 macOS 继续留在后续版本。

## 自动门禁

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cd apps/windows-ui
bun test
bun run build
cd ../..
python scripts/verify-windows-release.py
python scripts/build-windows-installer.py
python scripts/create-windows-acceptance-record.py --operator "待真实 Windows 验收"
python scripts/verify-windows-resilience.py --soak-seconds 10
python scripts/verify-managed-relay.py
python scripts/audit-managed-diagnostics.py
python -m unittest discover -s scripts/tests -p "test_*.py"
python scripts/check-windows-release-ready.py --manual-json dist/windows/windows-acceptance-record.json
python scripts/package-windows-candidate.py
```

### 最近一次自动门禁记录（2026-08-15）

- [x] 当前候选 manifest 已通过前端 177 项测试、Vite 构建、Rust fmt/Clippy/workspace 测试、Python 75 项脚本测试、Windows release verification 和安装包构建；连接快照、主机生命周期竞态、字体契约、强制配色、1920/2560 宽屏、“更多”菜单键盘导航和无效远程输入隔离回归测试已纳入门禁。
- [x] Windows CI run `31840782145` 全部通过：GDI/显示器捕获、H.264 High Profile/4K 探针、主机异常恢复、电源恢复、本地媒体 soak，以及公网中继目录/Noise/视频/输入/重连 E2E；候选 artifact 的精确源码 SHA 以同批 manifest 和 readiness 报告为准。

- [x] 安全扫描 run `31840782167`：Cargo advisory audit、Rust CodeQL、JavaScript/TypeScript CodeQL 全部通过。
- [x] Backend services CI run `31755376304` 通过；Managed relay monitor run `31755577111` 与 Managed diagnostics monitor run `31755578886` 均通过。
- [x] 签名候选 readiness 明确传入 `--minimum-soak-seconds 300`，不会因为默认值变化而把正式长稳门禁降级为普通 CI 的 10 秒。
- [x] 签名候选工作流已强制 300 秒原生 resilience soak，并由 `test_windows_workflow_policy.py` 防止回退到普通 CI 的 10 秒快速门禁。
- [x] 候选物料已由 Windows CI manifest 完成来源绑定；正式验收或发布时必须直接读取对应 run 的 artifact manifest，不把动态 SHA 回写到源码，避免证据提交改变被验证的 source commit。
- [x] 发布脚本与签名工作流现在都会严格校验 readiness 报告，并把该报告作为 Release 证据上传；当前候选 `ready: false`，不会被误发布为正式版本。
- [x] GitHub Actions 已统一到 `actions/checkout@v7`、`actions/upload-artifact@v7` 和 `actions/download-artifact@v8`，旧的 Node.js 20 deprecation warning 已不再作为当前候选的工作流风险。

### 先前自动门禁（2026-07-28）

- [x] Rust workspace：`cargo fmt --all -- --check`、`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`。
- [x] Windows 发布验证：`python scripts/verify-windows-release.py`（155 个前端测试通过，TypeScript/Vite/Rust release 构建通过）。
- [x] Windows 安装包构建：`python scripts/build-windows-installer.py`（安装包清单生成，当前 `signed: false`）。
- [x] 主机 DirectLan 接入回环：`cargo test -p desklink-windows --lib runtime::direct_video_tests::host_acceptor_keeps_authenticated_direct_datagram_connection`。
- [x] DirectLan 回环边界夹具：成功传输、候选过期、认证绑定失败和主动关闭；`cargo test -p desklink-transport direct_probe::tests`。
- [x] 直连质量可观测性：记录当前路径、最近 RTT、丢包基点和回落原因；`cargo clippy --workspace --all-targets -- -D warnings` 与 Windows UI 控制器测试通过。
- [x] 中继探测：`python scripts/verify-managed-relay.py`（双向控制探测通过；实际耗时以同批 `managed-relay-verification.json` 为准）。
- [x] 云诊断审计：`python scripts/audit-managed-diagnostics.py`（公网 health、服务、定时器和报告新鲜度通过）。
- [x] 变更检查：`git diff --check`。
- [x] 增加统一 Windows 发布预检：`check-windows-release-ready.py` 汇总来源 SHA、安装包完整性、签名、tag、中继/诊断证据，并把真实双机验收明确列为人工阻塞项。
- [x] 增加候选绑定的人工验收记录模板：版本、来源 SHA、安装包 SHA-256、操作者和 UTC 时间必须与预检候选一致。
- [x] Windows CI 与签名工作流上传统一发布预检报告，保留来源 SHA、安装包完整性和未完成阻塞项的构建证据。
- [x] 账号服务 CI 纳入 `server/account` 测试与类型检查；账号服务仍可不部署，Windows 本机模式不依赖它。

## 当前已知事实

- 中继实况探测已通过：`101.35.246.159:4433`。
- 本地诊断服务、定时器、公网诊断 health 和 Windows 脱敏 HTTPS 上报已通过审计；服务器诊断当前发布为 `8d92cdaee6d3`，最近一次 Nginx 配置备份在 `/etc/nginx/conf.d/p2p.yxswy.com.conf.bak-desklink-1784743477`。
- 当前候选物料以本地 `dist/windows/windows-installer-manifest.json`、`windows-release-verification.json` 和 `windows-release-readiness.json` 为准；安装器仍未签名，正式验收时必须读取同批清单中的 source commit 与 SHA-256，不使用旧文档中的动态值。
- 当前候选版本变更边界已整理到 [CHANGELOG.md](CHANGELOG.md)；精确候选提交、安装器 SHA-256 和中继镜像来源只以同批 manifest/readiness/审计报告为准，尚未形成正式发布 tag。
- 当前 `main` 已推送直连诊断、回落改动、DirectLan 回环夹具、近期画质窗口、一键恢复动作以及签名候选 300 秒 soak 门禁；尚无本地 `v*` 发布 tag。

</details>
