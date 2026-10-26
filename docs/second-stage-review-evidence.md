# 新增审查条目的归属与反证记录

2026-10-26；DL2-042 的证据分类记录，**不是044～061全部确认或最终能力矩阵**。这里只记录已核对的归属和反证；其余条目由对应owner对照当前分支验证后再分类，不按旧main重复实现。

## 主目录草稿归属

工程主会话及两个子agent均确认没有写入 `E:\desk-link\TODO.md`、没有新增044～061。工程编辑与提交均在独立 worktree `C:\Users\18267\.codex\worktrees\dl2-engineering\desk-link`；子agent只直接编辑该worktree的 `scripts/tests/test_current_source_versions.py` 和 `scripts/tests/test_verify.py`，另生成本地报告/缓存。Git共享元数据位于主仓库，不等于编辑了主checkout的TODO。

只读取样时，主目录TODO有156行新增、16行删除；SHA-256为 `754447427E7617CFA4C75AE4CD8519567773A717CEB1FF668AEED495567696E8`。内容有“新增全部确认”及052=P0描述，真实写入者尚未确认。文件时间、差异和hash都不能识别作者；本组没有代stage、覆盖、回滚或修订该未提交稿。该hash只是取样身份，其他并行会话后续可继续改变内容。

## 已核对的分类边界

| 条目 | 实际源码/已有修复 | 本轮分类与后续证据 |
| --- | --- | --- |
| DL2-044 | Cargo.lock锁定quinn0.11.11 / quinn-proto0.11.16；send_datagram用drop=true入队，拥塞可淘汰旧未发包后返回Ok。公开SendDatagramError只有UnsupportedByPeer/Disabled/TooLarge/ConnectionLost，无QueueFull；wait把内部Blocked保留为Pending。DeskLink cursor仍经?传播真实编码/seal/能力/尺寸/连接错误，peer编排select会接受其返回 | 原“队列满立即QueueFull、一次拥塞结束会话”不成立。剩余为可选cursor通道的真实错误分类/降级与作用域回归；严重性依据媒体实际测试，不沿用原拥塞P1论证。发送observer只能记录尝试/返回错误，不能记作真实丢包 |
| DL2-045 | main `a905b88` 的 `accept_host_direct_connection` 已是 `let incoming = endpoint.accept().await?; let Ok(connection) = incoming else { continue; };`；`?`传播的是外层Option的None，Some(Err)继续循环 | “任一accept Err永久禁用直连”的原P1叙述不成立。剩余是None终态的可观测性，以及失败握手后仍能接收下一连接的恢复回归；不能把原实现已正确的分支重复算作修复 |
| DL2-046 | main原出站探测曾为脱离生命周期的spawn；shared-probe独立提交 `3277c2a41af0ac923e1daa1c5cab5d952b355b9b` 已用JoinSet管理取消树，本工程复用为 `9c4119581508fc08c895aa8e3b24063ad03e657e` | 取消树部分与完成仲裁/活动连接覆盖分开。当前工程已有前者；后者随后由媒体165fcca切片处理，65c冻结handoff列046主项完成。本工程未引入或复跑后者，不能由初始JoinSet推导后续仲裁已在本工程接通，源交接完成也不代joint验收 |
| DL2-048 | `NoiseInitiator::receive` 把auth与session_key放入message3 payload，经Snow.write_message；responder先经read_handshake_message/Snow.read_message再拆出key。vendored Snow握手payload经过decrypt_and_mix_hash，带key时调用decrypt_ad并混入hash。Ed25519输入未直接包含key不等于key是网络可替换的未认证明文字段 | 现有证据不足以确认为P1漏洞，未发现已复现的网络替换/UKS路径；按熵源、派生与防御纵深评估待证。授权端点知道最终密钥，单独展示恶意端点选择/泄露它不足以证明外部攻击者突破认证。不据此直接改crypto |

044逐层核对本工程 `runtime.rs::send_cursor_loop` / peer select、transport `quic.rs::send_datagram` 及锁定缓存源码：quinn的 `connection.rs::Connection::send_datagram`（436）、公开错误枚举（1295）、`SendDatagram::poll`（837），quinn-proto的 `connection/datagrams.rs::Datagrams::send`（38）。与[版本化Connection API说明](https://docs.rs/quinn/0.11.11/quinn/struct.Connection.html#method.send_datagram)一致：非等待入队可淘汰旧包，wait在拥塞时等空间。这里核对的是API/实现语义，不是已运行的网络故障复现。

045在main基线和本工程源码均逐项读取确认；046区分main旧实现与本工程已复用的独立提交。048只读检查位于 `crates/desklink-crypto/src/noise.rs` 的消息3构造、接收、signature输入，及 `vendor/snow-0.9.6/src/handshakestate.rs` / `symmetricstate.rs` 的payload解密路径；这不是完整密码协议安全证明，也没有声称已完成新篡改或UKS测试。

044后续由媒体owner分别运行：实际localhost datagram/光标链路、禁用或不支持datagram的对端、有效上限/超限、真正连接终态，以及拥塞时非等待淘汰与wait等待的行为。真实连接关闭不能伪装成可恢复cursor丢包，可选通道能力/尺寸错误也不能与拥塞混为一类。需要观察peer/其他通道状态，不能只凭一个Err构造器宣称用户流程已修复。

observer可独立计调用尝试、返回错误及白名单错误类别；Ok既不是送达确认，也不保证没有内部淘汰。错误数不能解释为真实datagram丢弃/网络丢包数，真实丢弃、接收缺口与网络原因须另有可核验证据并分开归因。本工程没有新增或运行这些媒体故障回归，不能替媒体组勾选044。

048后续有意义的验证应包括：消息3密文/密钥payload篡改被拒、跨会话/不同身份的握手拼接被拒、错误expected peer拒绝、随机源失败与派生策略的明确语义；将网络攻击者、恶意已授权端点和熵源失效分别建模。只有独立攻击路径/回归证据支持后才能调整缺陷严重性。

## joint只读发现：DL2-021初始化窗口

协调方独立review并复读确认了joint的保留缺陷。本工程仅查看固定媒体提交 `89bd1e5161653dd668d290a417bd5f6cc146290a` 的 `apps/windows/src/transfer.rs`，没有读取或修改joint未提交树，也没有运行该新增回归。

`IncomingFileWorker::spawn_with` 的磁盘线程在create/open成功后取得metadata，约175～177行的 `ready.send(Ok(metadata)).is_err()` 直接return。若调用方future在 `opened.await` 期间因断连被drop，receiver已关闭；已有resume partial打开后仍带 `remove_on_drop=true`，线程return会触发IncomingFile::drop删除partial。约212～214行的正常commands关闭路径会preserve，但初始化早退没有进入该路径。IncomingFile::preserve关闭文件并置remove_on_drop=false；Drop在true时remove_file，形成完整源码因果链。

旧89源码上的初始化保留缺口已有媒体独立修复 **`65c70fd658d0526a8a27b75fdbf120ae065222bf`**，本工程只读Git确认其parent `6dbe53d64c60bbdd50af4b592ebef7471b800fcc`，只改TODO+transfer；ready发送失败改为同磁盘线程file.preserve后退出。来源TODO和协调方报告transfer28、lib/tests严格Clippy通过。后续joint578已保留65c，原日志588/589/597/598行分别记录四项初始化相关回归通过；当前分类更新为**源修复及局部joint回归已核对、第一波完整聚合待验**。本工程未自行复跑joint。

新增回归源码使用真实spawn_with/create和open后、读取/哈希前barrier，poll Pending后drop创建future，释放磁盘线程并等实际退出；9次取消后原4字节prefix、offset/hash/元数据不变，随后完整续传校验通过。另覆盖空part保留、显式cancel清理、初始化TooLarge不删旧prefix、Windows真实sync_data失败不发布。正常drop与显式cancel语义分开，未提高生产8线程上限；保留失败不发布完成或宣称成功保留。后续J已取得对应原始报告；第一波完整聚合仍须在实际候选执行适用套件，保留其精确源码身份。

此前协调方另报告joint未提交、索引有UU、测试文件未跟踪，这是当时的审查快照，不能当stable或直接当作后续当前状态；本工程未自行检查该WIP。其shared日志汇总更正为174通过/1忽略，先前274是撤回的笔误；这些都是协调方日志信息，尚未绑定最终joint提交，不是本工程复跑结果，也不是联合验收已完成。

052/053由网络组优先复核，054～058由服务组复核，其他项由媒体/产品/Apple按各自范围核验。未完成归属与成立性确认的项不加入本工程最终能力矩阵；主目录“新增全部确认”的总括应由原编辑者逐项更正，工程组不整份覆盖或stage该草稿。

## 收口来源与旧MacHost失败归因更新

本工程只读核对产品 `27fec756ba97ee6f67228dc9530c7b0578a49ff3` 的parent确为 `16594c4f8780cd565afd57695f88e29277ec5939`。代码唯一变化是提取 `InputSendFuture<Writer>` 类型别名并用于PendingInputSend.future，未改变发送、锁、取消或队列语义。提交TODO声明UI库135通过/1项签名资产忽略，本包 `--lib --tests --no-deps` Clippy通过；这是产品来源报告，本工程未复跑，不代表workspace严格检查或最终joint通过。

协调方报告Apple最新源分支完整FFI71项、Python186项与双头检查实跑通过，013 worker owned future仍在进行；本次未给该报告的完整源码SHA，本工程不擅自绑定所见动态branch head。工程 `d6f153e` 的旧FFI Host8通过/2失败、旧joint的两个MacHost失败均属于未整合完整Apple ABI/Host链的快照，不能外推到Apple当前源分支。等待Apple最终稳定SHA和完整依赖一起整合，保留已有等价API/Sender扩展并去重，不单独flat-pick9ad；来源通过仍不能替代joint实际复跑、Swift/Xcode或真实Mac presented。

最终joint已交付5782341，parents确为27fec+65c，保留原问题历史和新联合测试；本工程读取固定Git对象及外部报告，不读取joint WIP。旧89bd仍是缺陷来源，65c为修复来源。协调方此前10份resolution/测试备份的报告保留历史身份，当前以最终tree、已跟踪测试与原日志为实际证据，完整结果见下节。

## 媒体冻结manifest与交接范围

本工程只读两份已交付文件：`E:/desk-link/.worktrees/dl2-targets/windows-media/validation/final-validation.json` 及 `windows-media-handoff.json`，没有读取或操作媒体/joint工作树。前者记录source_revision及source_revision_after均为 `65c70fd658d0526a8a27b75fdbf120ae065222bf`、working_status为空、Cargo.lock SHA256为 `4a3df4b7fc92340f5296c0691eb77c78ae8d45d689f1d2b323aec7fb371427f6`，与该固定Git对象Cargo.lock原始bytes计算结果吻合；源HEAD/干净工作树另由协调方确认。本工程读取报告并核对固定Git对象及日志，不把报告字段当作自行实跑或对动态工作树的当前检查。

三组来源测试分别为Windows lib/tests 266通过/17忽略、protocol/crypto/transport/video/session 177通过/1忽略、FFI controller_runtime 5通过，合计448通过/0失败/18忽略。本工程重新合计final-1/2/3原日志的test-result行，分别17/19/1条，与manifest summary及handoff totals吻合；零测试行未增加通过数。其余三项为workspace/all-targets check、指定windows/transport/protocol/crypto/video包lib/tests严格Clippy、workspace fmt，退出码均为0；不是完整workspace Clippy或完整FFI/Swift/Bun/安装包结果。交接没有执行interactive desktop/live relay/physical soak忽略项，未运行真实远控/输入注入或发布。

本次读取文件的SHA256：final-validation为 `9657b095b2050e17c1a1f96b33710db7504799b57bf6a8b90c074380a8d8b67e`；handoff为 `8a0f884575dcc42060bdddc4c08326469efaf6f226d513ddc6d3ff7fb5261bfb`。哈希仅绑定这次报告快照，文件路径后续可变化，不把它们当最终joint证据。

handoff列37笔唯一提交，本工程核对其SHA/subject/ancestry均与Git对象吻合，集合精确覆盖main基线a905b88至65c的37笔提交，已包含三条imported contract，不额外再加三笔。仅020/046/050主项标完成，015/030明确部分实现，其他11项仅切片完成并有pending。015仍CPU读回，GPU缩放/NV12 surface/MFT设备绑定/租借池未实现；030仍256MiB，无大文件/4GiB、目录或持久块证明。046源分支的后续165fcca所有权/迟到结果切片不得与本工程已复用的初始3277取消树混为一笔。041/013和整包可交付性继续以最终joint及Apple完整稳定聚合的实际证据为准，不据448/18或37提交数关闭。

## Windows最终joint578的固定对象核对

`5782341e22ba55b2efe30a8eeaeafbebfe0daaba` 的tree为 `59c7270aea76bc1d542ac49c625e308b42ef20f2`，parents依序为27fec756与65c70fd；27笔产品及37笔媒体提交都保留为祖先。外部 `E:/desk-link/.worktrees/dl2-targets/windows-joint/handoff-final.json` 本次SHA256为 `5bd84ffebfb3f0953df7d899850f904e77358e032f37542fc4c90a2f1e9c58dd`。六份日志/JSON哈希均匹配；固定Git blob中的 `docs/windows-joint-integration.md` 哈希为 `8da83667d95cd5ee38aed2294aa770888388e9a4faebba9be46f18a99a724517`，与报告一致。clean/unmerged字段属于该次来源记录，本工程未访问动态index。

本工程对原Rust日志38条summary与final-test-counts逐条核对：634通过、2失败、19忽略，Cargo101/包装器1；失败均为已列的FFI host_runtime两项，固定源码host_worker.rs:896仍input_epoch=0，完整Apple ABI/Host链尚未合入。该结果属于该tree，不能写Rust全绿，也不能外推Apple当前源分支失败。Bun1.3.14原日志474通过/0失败、71文件/1894断言，构建成功；八包lib/tests严格Clippy未用no-deps、来源exit0，限定范围保留。

联合测试windows_joint_tests.rs确为tree blob并由runtime接入；477个源码快照逐项与Git blob SHA256吻合，测试候选tree至最终tree只补TODO/报告，可执行源码未变。本工程另检索到两项013软件MFT/QUIC/Noise/输入guard联合用例及四项021初始化回归为ok。真实WebView/compositor、系统输入注入、双机、硬件MFT/拓扑矩阵、完整Apple ABI、GUI observer闭环仍未运行。本次是源码/原日志核对，未冒充本工程实跑。

工程12b54ce与J578的Git merge-tree影响预演产生5个需人工核对的冲突路径：TODO.md、Windows runtime/runtime_contract、transport direct_probe及013协议文档。预演不修改工作树、没有执行候选测试；后续独立第一波候选须保留J已解冲突与等价patch去重，实际处理这些路径，不把预演当合并完成。
