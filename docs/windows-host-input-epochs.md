# Windows Host 输入 epoch（DL2-013）

主机实际发布的 VideoConfig 是控制端上下文来源。配置携带 `(stream_id, input_epoch)`，ControllerRuntime 发送入口必须保留事件产生时的 InputContext；没有“发送时补当前值”、零值或固定1兜底。生产 HostVideoPipeline 的 prepare 需要明确的非零 epoch，编码队列逐帧携带其采集映射的上下文。

Windows 进程使用一次 `OsRng.try_fill_bytes` 取得完整64位非零随机起点。全进程共享 OnceLock allocator，首个分配值就是该起点；后续每个新认证会话和有效输入边界使用checked单调分配。零随机值、RNG错误和u64耗尽都明确失败，不掩码、不截断、不重抽、不wrap、不回落1。重启隔离依赖随机起点，不能宣称持久化全局唯一；该epoch用于旧新输入绑定，不是密钥或AEAD nonce。Apple/FFI可复用相同规则，C/Swift保留u64，JavaScript IPC需规范十进制字符串和BigInt。

活动输出、坐标映射或有效性变化时，固定锁顺序为 SelectedDesktop → InputInjector。暂停、释放旧按键、推进epoch和提交新映射在同一映射锁下协调；释放受阻保持暂停和旧发布状态，下一轮重试，不因一次输入阻塞触发断线风暴。列表名称/主屏元数据变化，以及纯质量/Profile/码率重配置保持输入epoch和已按下状态。

收到输入时，在映射锁内先用完整wire context匹配，再更新顺序/时间状态或触及注入器。因此旧移动、按下、Unicode文本按键、鼠标/键盘释放和由ReleaseAll展开的旧释放都不能影响新epoch。主机主动结束和切换所需的release是独立本机动作；peer退出guard即使捕获线程仍持有injector Arc，也会关闭输入并释放旧状态。

异步编码可能返回旧输入的输出。固定容量32项的(frame_id,config_version)绑定保存接受输入时的采集上下文，配置或上下文变化即清除旧绑定；无法匹配、重复或跨几何返回的旧输出被丢弃并请求IDR，不重新标注最新epoch。编码器的单样本提交身份当前是该绑定来源，调整性能采样时必须保留这份领域身份。

自动验证使用记录/阻塞注入后端和合成编码帧，取实际HostVideoPipeline编码再解码的VideoConfig上下文。覆盖A→B→A、旧release不释放新按键、相同持久stream的新会话/进程、旧高sequence不污染新排序、质量/Profile不换epoch、旧MFT输出跨几何、释放受阻与退出，以及RNG/零值/溢出。没有建立真实共享或注入真实输入。

```powershell
$env:CARGO_TARGET_DIR = 'E:\desk-link\.worktrees\dl2-targets\windows-media'
python scripts/run-windows-cargo.py test -p desklink-windows --lib --test runtime_contract --locked
python scripts/run-windows-cargo.py clippy -p desklink-windows --lib --tests --locked -- -D warnings
```

仍需各控制端首帧实际呈现后放行的完整跨组回归、FFI被控Mac配置接线，以及真实热插拔、旋转、DPI、睡眠和双机验收；本Host切片不能单独当作当前整包可交付证据。

每个输入epoch的发布还绑定实际编码器配置版本。Coordinator仅接受 `publish(snapshot, encoder.config_version())`；同一活动上下文几何/有效性边界必须使用大于epoch出生版本的真实配置版本，随后发布配置及IDR。纯编码调整可以推进config_version而不推进epoch。HostPipeline拒绝版本倒退、同版本换epoch/尺寸；ControllerRuntime本来已拒绝同stream+同configVersion但配置字段改变，此约束保留，没有绕过它使旧帧误激活。u32版本耗尽明确退出当前peer，新stream再建上下文，不wrap复用。

同尺寸/同SPS选屏、虚拟原点变化回归使用实际SoftwareOnly H264Encoder重配/getter取得版本，配置由实际HostPipeline编码/解码取token；不通过给fixture填常量把不一致掩盖成通过。
