# Windows 捕获 staging 复用验证

DL2-015 的这个切片只复用进程持有的 staging texture 和 immediate context。缓存按 D3D11 device 身份及完整 texture 描述（尺寸、格式、mip/array/sample 等）校验；设备或布局改变时重新创建。Map、复制及 Unmap 仍在 `ReleaseFrame` 前完成，桌面复制 surface 不会跨越其有效期。GPU 缩放、GPU NV12 转换、surface 编码输入和 CPU 输出 buffer 回收仍待后续实现。

正确性用例在本机 WARP 软件 D3D11 device 上创建合成 BGRA texture，不捕获桌面、不建立远控连接、不注入输入。覆盖复用后读取新内容、尺寸及 device 改变时重建、padding/row pitch 边界。

```powershell
$env:CARGO_TARGET_DIR = 'E:\desk-link\.worktrees\dl2-targets\windows-media'
python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked capture::readback::tests
python scripts/run-windows-cargo.py test -p desklink-windows --lib --locked --release capture::readback::tests::staging_reuse_benchmark -- --ignored --nocapture
```

基准对 1080p 与 1440p 合成静态 BGRA 输入各做 5 次预热及 60 组 fresh/reuse 对照，输出 p50/p95/p99、staging 创建次数及明确的每帧 CPU copy 字节。fresh 路径每帧创建 staging；reuse 路径缓存 1 张，两个路径仍各自分配返回的 CPU BGRA buffer。先运行普通测试后再显式运行 `--ignored` 基准，避免把交互桌面 smoke 一起启用。

每次比较保存完整命令、Git revision、dirty diff、CPU/Windows 版本、电源/温度状态与输出。优化构建至少重复三轮；未优化构建只可作为基准工具烟测，不能与 release 对照。WARP 是 CPU 软件实现，这些计时不能代表 Intel/AMD/NVIDIA、混合 GPU、实际桌面捕获延迟或整条媒体链的收益。当前已经验证的是安全的 staging 复用和可重复工具；真实 GPU 时间、分配率、功耗、帧龄和 GPU 常驻链路仍需单独验收。
