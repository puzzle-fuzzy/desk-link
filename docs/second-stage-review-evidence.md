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
| DL2-046 | main原出站探测曾为脱离生命周期的spawn；shared-probe独立提交 `3277c2a41af0ac923e1daa1c5cab5d952b355b9b` 已用JoinSet管理取消树，本工程复用为 `9c4119581508fc08c895aa8e3b24063ad03e657e` | 取消树部分与完成仲裁/活动连接覆盖分开。当前工程已有前者；后者继续由媒体owner用受控完成顺序回归验证，不能把已修部分再报一遍，也不能由JoinSet存在推导完成仲裁已正确 |
| DL2-048 | `NoiseInitiator::receive` 把auth与session_key放入message3 payload，经Snow.write_message；responder先经read_handshake_message/Snow.read_message再拆出key。vendored Snow握手payload经过decrypt_and_mix_hash，带key时调用decrypt_ad并混入hash。Ed25519输入未直接包含key不等于key是网络可替换的未认证明文字段 | 现有证据不足以确认为P1漏洞，未发现已复现的网络替换/UKS路径；按熵源、派生与防御纵深评估待证。授权端点知道最终密钥，单独展示恶意端点选择/泄露它不足以证明外部攻击者突破认证。不据此直接改crypto |

044逐层核对本工程 `runtime.rs::send_cursor_loop` / peer select、transport `quic.rs::send_datagram` 及锁定缓存源码：quinn的 `connection.rs::Connection::send_datagram`（436）、公开错误枚举（1295）、`SendDatagram::poll`（837），quinn-proto的 `connection/datagrams.rs::Datagrams::send`（38）。与[版本化Connection API说明](https://docs.rs/quinn/0.11.11/quinn/struct.Connection.html#method.send_datagram)一致：非等待入队可淘汰旧包，wait在拥塞时等空间。这里核对的是API/实现语义，不是已运行的网络故障复现。

045在main基线和本工程源码均逐项读取确认；046区分main旧实现与本工程已复用的独立提交。048只读检查位于 `crates/desklink-crypto/src/noise.rs` 的消息3构造、接收、signature输入，及 `vendor/snow-0.9.6/src/handshakestate.rs` / `symmetricstate.rs` 的payload解密路径；这不是完整密码协议安全证明，也没有声称已完成新篡改或UKS测试。

044后续由媒体owner分别运行：实际localhost datagram/光标链路、禁用或不支持datagram的对端、有效上限/超限、真正连接终态，以及拥塞时非等待淘汰与wait等待的行为。真实连接关闭不能伪装成可恢复cursor丢包，可选通道能力/尺寸错误也不能与拥塞混为一类。需要观察peer/其他通道状态，不能只凭一个Err构造器宣称用户流程已修复。

observer可独立计调用尝试、返回错误及白名单错误类别；Ok既不是送达确认，也不保证没有内部淘汰。错误数不能解释为真实datagram丢弃/网络丢包数，真实丢弃、接收缺口与网络原因须另有可核验证据并分开归因。本工程没有新增或运行这些媒体故障回归，不能替媒体组勾选044。

048后续有意义的验证应包括：消息3密文/密钥payload篡改被拒、跨会话/不同身份的握手拼接被拒、错误expected peer拒绝、随机源失败与派生策略的明确语义；将网络攻击者、恶意已授权端点和熵源失效分别建模。只有独立攻击路径/回归证据支持后才能调整缺陷严重性。

## joint只读发现：DL2-021初始化窗口

协调方独立review并复读确认了joint的保留缺陷。本工程仅查看固定媒体提交 `89bd1e5161653dd668d290a417bd5f6cc146290a` 的 `apps/windows/src/transfer.rs`，没有读取或修改joint未提交树，也没有运行该新增回归。

`IncomingFileWorker::spawn_with` 的磁盘线程在create/open成功后取得metadata，约175～177行的 `ready.send(Ok(metadata)).is_err()` 直接return。若调用方future在 `opened.await` 期间因断连被drop，receiver已关闭；已有resume partial打开后仍带 `remove_on_drop=true`，线程return会触发IncomingFile::drop删除partial。约212～214行的正常commands关闭路径会preserve，但初始化早退没有进入该路径。IncomingFile::preserve关闭文件并置remove_on_drop=false；Drop在true时remove_file，形成完整源码因果链。

该项分类为**源码确定的初始化保留缺口，修复/确定性回归待媒体owner交付**，不是已经跑失败的本工程测试。须用真实临时partial和可控初始化barrier覆盖“文件已打开、ready未交付→drop create future→释放磁盘线程”；断言原prefix内容/长度/hash与后续resume仍可用。正常handle-drop保留和显式cancel清理分别回归，不把两种语义合并。媒体独立021修复及结果必须先复用到joint，再取得最终revision与报告；不靠一次旧suite通过代替这个未覆盖窗口。

协调方另报告joint仍未提交、索引有UU、测试文件未跟踪，不能当stable；本工程未自行检查该WIP。其shared日志汇总更正为174通过/1忽略，先前274是撤回的笔误；这些都是协调方日志信息，尚未绑定最终joint提交，不是本工程复跑结果，也不是联合验收已完成。

052/053由网络组优先复核，054～058由服务组复核，其他项由媒体/产品/Apple按各自范围核验。未完成归属与成立性确认的项不加入本工程最终能力矩阵；主目录“新增全部确认”的总括应由原编辑者逐项更正，工程组不整份覆盖或stage该草稿。
