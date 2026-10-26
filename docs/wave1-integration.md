# 第一波协议13集成证据

日期：2026-10-26。已测试源码为 **`a4c5b404d43794d646394923a481f23c9298c3ed`**，tree为 `154d51e9258a7731d78e78e1d7462bba475c0e36`，执行时工作树干净。后续收口仅修改Markdown，最终交接manifest另绑定文档提交与该测试源的差异。应用0.1.93、业务协议13、Bun1.3.14、Rust1.97.1；main仍为a905b88/协议12，未合入、推送或发布。

## 完整来源与集成修复

各来源通过正常merge保留完整历史，未覆盖源分支或main的未知未提交TODO。

| 来源 | 固定提交 |
| --- | --- |
| Windows产品/媒体joint | `5782341e22ba55b2efe30a8eeaeafbebfe0daaba`，parents为27fec756及媒体65c70fd |
| Apple/FFI | `f954a68153168a1ded048ddf93eb9c565684d096` |
| Apple输入receipt/admission后继 | `cb7514cc29b7a94ad40ac00f0a5b98936b7d5be5` |
| 网络资源/期限/关闭 | `4b15561586fab7a1481ba6dd4ff4854f34123018` |
| 工程040/041/042 | `6b31360ce4dc40d0f1715ce1e40c000788a6b812` |
| GUI044/045 | `5069158584dfa1f729defe2fadb5b9933acdfcff` |
| 原生诊断u64 producer | `28c11ee05a08183f7636ede7946e93029c7bbb16` |
| services最终schema/预算/分桶/u64 | `d2d4f1c4c4942df03eda71e8aa1ebf3f8ab22c3a` |

冲突解决保留Windows非零随机epoch、捕获前context及编码输出身份映射、Apple完整ABI/发送所有权、网络有界资源与期限。新增实际集成修复分别提交：1daf916的受限flow-control adapter拒绝超预算、不接受不透明TransportConfig；32e13e2在内存中准备MSVC编译环境并实际编译C ABI；bd5e166把三个测试observer改为容量1有界队列；a4c5b40扩大relay入口到全部lib/tests。040共享错误分类的职责拆分保留Host/Controller的三项既有语义差异，不改变重试预算。

## 实际软件检查

权威报告为 `dist/verify/wave1/final-software-v3.json`：**25个检查全部通过，passed=true**。检查包括依赖安装，不能称为25个测试；下列Rust范围有重叠，不能相加成去重覆盖率。

| 范围 | 实际结果 |
| --- | --- |
| 三个Bun项目 | 前端474、账号79、诊断129通过；各自类型检查、frozen安装及前端生产构建通过 |
| Python | 257通过 |
| Rust workspace lib | 511通过、3忽略 |
| Windows完整tests | 292通过、17忽略 |
| Windows UI lib | 141通过、1忽略（也在workspace范围内） |
| FFI完整tests | 91通过、0忽略，包含实际MSVC C布局编译 |
| relay全部lib/tests | 64通过、3忽略，含资源、读期限、地址族与关闭回归 |
| 静态门禁 | workspace fmt、all-targets/all-features check及严格Clippy通过 |

重跑入口：

```powershell
python scripts/verify.py fast integration --bun D:/SoftwareCache/npm/_npx/60c3515df86f25b1/node_modules/bun/bin/bun.exe --target-dir E:/desk-link/.worktrees/dl2-targets/engineering --report dist/verify/wave1/replay-software.json
```

Cargo经Windows wrapper串行执行；不要与其他Cargo任务共用该target并发运行。旧 `initial-merge-check.json`、`merged-ffi.json`、`final-software.json` 失败及v2报告全部保留。5501/56f/d6和旧joint的零epoch失败属于对应历史源，当前完整FFI91通过不改写旧结果。

## 原生签名到HTTP与持久存储

在干净a4上实际执行production `build_request_from_log` 所在原生fixture测试，使用合成身份和独立临时日志；新导出文件SHA256为 `a0b66b3baf45bb81b264af1a36250ec490b85ffb13f87e496cff85fb077c22d7`。生产服务模块直接接收原始body/hash/Ed25519签名，监听127.0.0.1临时端口，写入临时文件SQLite并关闭重开验证。没有读取用户日志或远端上传。

`fresh-native-consumer-result.json` 的 **19案例全部通过、differences为空**：12项当前/历史规范化正例、2项旧安全数值接受、4项旧不安全Number拒绝且零存储、1项body篡改并重新hash但保留旧签名后401。正例验证精确值与原类型、重开、去重及分析隐私。新合同覆盖7事件、24个event/field对、18个不同u64键；native仍u64，JSON为有界规范十进制字符串。旧安全Number保留原类型；不安全旧记录若在所选批次预算内会使整个批次Encoding失败，不宣称自动逐条隔离。

证据脚本 `fresh-native-consumer.ts` 的SHA256为 `4bb02e42c14daf7e455e2f72bdda4de49d5a5eb91cf155899fdc6d72befc0060`，报告绑定9个实际生产模块的哈希。服务时钟通过既有测试接口固定到fixture最大事件时间以控制retention。当前6组事件时间每次生成变化，因此签名/body不能跨生成强求字节相同；只在语义比较中排除timestamp，仍逐一验证各自原始签名。历史body字节保持一致。旧legacy生成来源的未提交测试接缝没有归档补丁，该历史生成器不能宣称精确重建；新28及a4生成路径可重现。

## 平台和性能边界

`platform-conditions.json` 为 **incomplete / passed=false：2通过、7跳过**。通过的是生成双头检查和CPU离线基线；未授权交互设备测试。CPU基线为合成1920×1080、120次转换，p50/p95/p99为4818/4987/5241微秒，encode=false、completed_outputs=0；这是分段离线开销，不能换算真实FPS、编码吞吐或GPU收益。

- [ ] Swift/Xcode及6项Swift Recording用例、Apple实际presented与同stream完整拓扑映射。
- [ ] 实际WebView、系统权限、真实输入注入、双机、显示器/分辨率变化与完整Mac功能。
- [ ] 真实捕获/编码、设备恢复、长稳、同条件画质与端到端性能、签名安装及正式发布。

GPU235保留为下一独立候选；文件030/协议14与LAN024后续生产接线另行集成。它们不改变本第一波测试源。

原始报告、失败历史、脚本与新fixture另复制到Cargo target之外的 `E:/desk-link/.worktrees/dl2-targets/engineering-evidence/wave1-a4c5b404d43794d646394923a481f23c9298c3ed/`，交接 `handoff.json` 记录最终文档提交、全部文件大小/SHA256和源历史祖先检查，清理target后仍可审计。
