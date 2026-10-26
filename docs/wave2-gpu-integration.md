# 第二波 GPU 集成证据

日期：2026-10-26。第二波在独立worktree完整merge GPU235，在干净97ff源码实际执行以下检查。第一波a4测试源与c3冻结提交不变；本报告随后的文档提交不改动已测代码，最终SHA/tree、元数据差异和文件哈希由外置handoff.json绑定。应用0.1.93、协议13、Bun1.3.14、Rust1.97.1；未合main、推送或发布。

## 1. 不可变源码与合并方式

| 项目 | 固定身份 |
| --- | --- |
| 第一波冻结基线 | `c3fa4df987fb88be08eb5aeea9c7983b556e87d3`；第一波外置124份证据保持原身份与范围 |
| GPU 来源 | `23526f23f450e64edea098a36b07ed4c62d4982b`，完整七笔历史 |
| 第二波已测试干净源码 | `97ff7c08199cdad4511b0044ddf783d79e2295b1` |
| 已测试 tree | `2319d0e73bb68c22496d43ec1fb99f84f4e1dd6f` |
| merge parents | `c3fa4df987fb88be08eb5aeea9c7983b556e87d3` + `23526f23f450e64edea098a36b07ed4c62d4982b` |
| 独立 worktree | `C:/Users/18267/.codex/worktrees/dl2-wave2-gpu-integration/desk-link` |

第二波从第一波冻结提交正常 merge 完整 GPU 历史，未以平铺拷贝或单独末笔文档提交替代源码链。第一波 J/A/CB/N/E/GUI/producer/services 的既有历史与语义继续保留；本次新增 GPU 软件候选，不回写第一波原始测试源码或报告。

GPU 独有来源历史（从早到晚）：

| SHA | 提交主题 |
| --- | --- |
| `2d79f9706303cb2dffa7abc764c8f7450591c3b6` | `feat(DL2-015): add bounded owned GPU surfaces and NV12 processing` |
| `c82ea4489f9c566fb2989ba32f042dedf5eb2906` | `style(DL2-015): normalize GPU module declaration order` |
| `e01afa1bc55f56c1e092ccc8b97bef5a40ce8eaa` | `feat(DL2-016): encode owned D3D11 surfaces with tracked sample leases` |
| `99fa9929699d7360b60ffa546527461711c145d9` | `fix(DL2-015): trim retired GPU generations after safe ownership release` |
| `50d721a404ab60efb692d928b185325f053c2492` | `test(DL2-016): decode representative GPU frame sizes and display apertures` |
| `6a60f451ec79e599817f666980811cb81994010b` | `perf(DL2-015): add source-bound synthetic GPU stage probes` |
| `23526f23f450e64edea098a36b07ed4c62d4982b` | `docs(DL2-015): record the frozen synthetic GPU stage matrix` |

## 2. 本轮范围与第一波保护边界

| 边界 | 本轮必须保留的语义 | 证据范围 |
| --- | --- | --- |
| 协议与共享输入合同 | 继续协议13；InputContext/stream/input_epoch、独立DirectProbe lane与既有输入受限反馈不因GPU接入重定义 | 完整25项软件检查及相应原始日志；不引入文件协议14 |
| 几何与编码配置 | GPU/CPU后端及纯编码切换保留不变的输入映射/epoch；实际几何或同尺寸原点变化按既有规则更新context及真实config_version，旧输出与旧release不能重新取得新context | 实际GPU runtime epoch合成回归；不等同真实热插拔/多屏验收 |
| 资源与取消 | owned BGRA、NV12 lease、fence及tracked sample按最终所有权释放；已输出或drop encoder不等于MFT已释放最后sample引用；取消/Host本机收回不受旧远端release拒绝影响 | pool/encoder/bitstream/runtime软件与显式合成GPU用例；不承诺中断阻塞驱动调用 |
| 失败回退 | 保留同一owned BGRA的CPU回读、软件IDR回退及真实配置版本边界；GPU可用性不能只由D3D11_AWARE或历史协商字段推断 | encoder及capability inventory范围单列；不能由一块适配器外推厂商矩阵 |
| 诊断完整u64 | 第一波规范十进制字符串、历史精确归一与原Rust签名链继续有效；7事件/24个event-field对/18个不同u64键不扩散 | 第二波重新生成native fixture并执行fresh本地HTTP/schema/SQLite验收，不复用第一波绿色结果冒充本轮执行 |

GPU surface路径仍有BGRA GPU复制、VP输出、query/view创建；已知计数不能覆盖driver/COM/heap全部分配，不宣称零复制。库中含明确opt-in的实验4K合成用例，不提升产品默认上限或正式4K支持承诺。

## 3. 完整软件门禁

干净源码 `97ff7c08199cdad4511b0044ddf783d79e2295b1` 的25项软件检查全部通过，严格 workspace check/Clippy/fmt通过。以下为命令输出分组，彼此可能重叠，不相加为去重覆盖量；ignored不计通过。

| 软件范围 | 实跑结果 |
| --- | --- |
| Bun 前端 / 账号 / 诊断 | 474 / 79 / 129通过 |
| Python | 257通过 |
| workspace-lib | 516通过、18忽略 |
| Windows alltests | 297通过、32忽略 |
| Windows UI | 141通过、1忽略 |
| FFI完整lib/tests | 91通过、0忽略，保留真实MSVC C ABI布局执行 |
| relay | 64通过、3忽略 |
| workspace check / strict Clippy / fmt | 全部通过 |

入口与执行证据：`python scripts/verify.py fast integration`，实际 `--bun`、target、report与全部子任务argv以 `dist/verify/wave2/software.json` 为准；原始日志位于 `dist/verify/wave2/software-logs/`。不能把本段理解为仅运行上面摘要中的几项选择集。

## 4. 六条显式合成 GPU 命令

本候选实际执行：六条命令分别8 / 6 / 4 / 1 / 1 / 4通过，合计24通过、0忽略。其中9项常规、15项原本ignored（包括硬件MFT capability inventory），本次通过显式选择实际执行；不能将普通软件套件里的32个ignored一并算作已运行。

执行器：`python dist/verify/wave2/run_gpu_checks.py`。执行器通过统一验证模块及Windows原生环境运行Cargo；`CARGO_TARGET_DIR=E:/desk-link/.worktrees/dl2-targets/engineering`，`CARGO_NET_OFFLINE=true`，目标范围为synthetic GPU/D3D11/MFT，未启用交互桌面捕获、原生输入注入或真实远控。

下列为六条等价原生wrapper命令，最终实际argv/耗时/返回码以 `synthetic-gpu.json` 和日志为准：

```powershell
python scripts/run-windows-cargo.py test -p desklink-windows --locked --offline --lib gpu::tests:: -- --include-ignored --test-threads=1
python scripts/run-windows-cargo.py test -p desklink-windows --locked --offline --lib encoder::gpu_tests:: -- --ignored --test-threads=1
python scripts/run-windows-cargo.py test -p desklink-windows --locked --offline --lib gpu::bitstream_validation:: -- --include-ignored --test-threads=1
python scripts/run-windows-cargo.py test -p desklink-windows --locked --offline --lib runtime::retry_policy_tests::gpu_media_backend_changes_preserve_epochs_and_same_size_geometry_rejects_old_outputs -- --exact --ignored --test-threads=1
python scripts/run-windows-cargo.py test -p desklink-windows --locked --offline --lib encoder::gpu_capabilities::tests::actual_hardware_mft_d3d11_capability_inventory -- --exact --ignored --test-threads=1
python scripts/run-windows-cargo.py test -p desklink-windows --locked --offline --example gpu_media_probe probe::tests:: -- --test-threads=1
```

| 分组 | 通过 / 忽略 |
| --- | --- |
| gpu-pools | 8 / 0 |
| gpu-encoder | 6 / 0 |
| gpu-bitstream | 4 / 0 |
| gpu-runtime-epoch | 1 / 0 |
| gpu-capability-inventory | 1 / 0 |
| gpu-probe-tests | 4 / 0 |

证据：`dist/verify/wave2/synthetic-gpu.json`、`dist/verify/wave2/synthetic-gpu-logs/`。inventory只枚举/激活MFT并读取属性，其通过不等于所有枚举硬件都能成功编码；surface实际成功由encoder/bitstream用例分别证明。

## 5. Fresh native producer与实际HTTP/SQLite

本候选实际执行：重新生成native fixture的原生用例1通过，随后真实本地HTTP consumer 19案例全部按预期通过。fixture SHA-256为：

`cb0621b16a33b7ae2716c5c06ed08ae31562d55e60c74b8f1b8602a597b76fbf`

正例直接使用原始RustSignedRequest的body/hash/签名，不以手写JSON、body重构或重签代替。覆盖schema、文件SQLite、关闭重开与去重；旧unsafe numeric批次仍拒绝，篡改负例仍须401。19案例通过不等于19次请求都应入库，也不证明真实GUI诊断送达、生产服务或公网可用。

本轮实际执行的辅助入口保存于ignored目录：`dist/verify/wave2/check_native_fixture.py`、`fresh_native_replay.py`、`fresh-native-consumer.ts`。外置证据包保存这些脚本、实际native命令、fixture、HTTP逐案例结果、重开/去重断言和日志哈希；以实际结果为证据，脚本存在本身不表示通过。

## 6. Platform/native/perf结果与剩余验收

本候选实际执行：2通过、7跳过，整体 `incomplete / passed=false`。通过项是双头同步检查与CPU离线基线；CPU离线基线绑定相同干净97ff源码，1920×1080、120次转换，p50/p95/p99为4763/5008/5239微秒；encode=false、completed_outputs=0，不能据此推导GPU编码吞吐或FPS。原报告为media-baseline.json。跳过不是失败，也不是通过。

- [ ] Swift/Xcode、Apple Recording、真实presented、系统输入与权限验收。
- [ ] 实际WebView、真实DXGI桌面复制、远控输入、双机、拓扑/热插拔、睡眠/恢复。
- [ ] Intel/AMD/混合适配器与真实driver/device-loss矩阵；不能由本机合成GPU执行外推。
- [ ] 同硬件完整前后性能、输入到显示、GPU执行时间、真实图像/文字质量、功耗温度和长稳。
- [ ] 安装签名、正式发布、线上部署与生产/provider验收。

第一波基线保护通过软件回归与显式合成用例，不改变第一波本来未运行的这些场景。第二波软件候选通过也不等同已合main或正式release；文件协议14与LAN024后续生产接线不进入本轮。

## 7. 证据目录与封存清单

原始证据复制到Cargo target之外的目录：

`E:/desk-link/.worktrees/dl2-targets/engineering-evidence/wave2-gpu-97ff7c08199cdad4511b0044ddf783d79e2295b1/`

| 证据 | 本地执行路径 / 最终封存要求 |
| --- | --- |
| merge审计 | `dist/verify/wave2/merge-audit.json`；保留完整parents、七笔来源、17个路径blob与13个受保护生产blob不变检查；本次自动合并无冲突 |
| 完整软件 | `dist/verify/wave2/software.json` + `software-logs/`；封存实际argv、版本、返回码、计数、source clean前后身份 |
| 显式合成GPU | `dist/verify/wave2/synthetic-gpu.json` + `synthetic-gpu-logs/`；保留每命令计数与硬件/后端输出 |
| fresh producer/HTTP | 实际fixture、native原日志、consumer逐案例JSON、辅助脚本及各自hash；fixture使用上列固定SHA-256 |
| platform与CPU离线 | 实际native/perf报告、跳过原因、CPU离线原始输出和source绑定 |
| 总清单 | handoff.json记录全部相对路径、bytes、SHA-256及总数；自身哈希见交接消息 |
| 最终元数据提交 | handoff.json绑定最终提交/tree及仅Markdown差异，测试源始终为97ff |

GPU来源已有的合成阶段矩阵继续绑定来源 `6a60f451ec79e599817f666980811cb81994010b` 及其外部 `windows-gpu/validation/matrix-6a60f45` 证据，235只是后继记录。它与本轮97ff软件/合成回归分别保留：原CPU wall分位数不是纯GPU时间，不是本轮重新测得的双机收益，也不证明质量等价或零复制。
