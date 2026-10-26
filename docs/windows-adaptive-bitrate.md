# Windows 档内自适应码率（DL2-023）

该切片把现有档位的码率单独调节，保留分辨率与帧率。只在 Automatic 偏好下启用；显式选择手动档位会清除旧码率请求，并在相同档位被选中时恢复其配置码率。没有扩展4K/60、帧率或内容模式能力。

| 档位 | 码率上限 | 档内下限 |
| --- | --- | --- |
| Sharp | 18 Mbps | 6 Mbps |
| Balanced | 2.5 Mbps | 833333 bps |
| Smooth | 1.5 Mbps | 0.5 Mbps |

不少于40包的反馈才构成网络窗口。两个退化窗口后降低20%，严重丢包降低35%；原生确认成功后冷却8个有效反馈窗口。12个健康窗口后增加上限的5%，达到上限前优先恢复码率。冷却是反馈窗口数，不代表固定秒数。低于下限、不支持动态修改或解码/新鲜度压力继续使用现有档位回退。

请求与实际值分开；只有原生事务确认的值进入策略。质量/Profile工作阻止新码率决策，并使旧码率请求失效，防止旧档位目标晚到。配置队列最多4类待执行各1项和1项进行中。等待真实原生应答的future由控制select独立轮询，并随peer取消；5秒本地计时不再冒充原生已拒绝。驱动内部阻塞无法由此强制中断。

拓扑/显示/Profile调整保留已确认码率；质量档位应用后回报真实档位及码率。动态事务回滚无法确认时，重建旧设置并请求IDR后再编码。当前本机软件MFT不支持动态设置，实际走已有档位策略；纯策略和事务用例通过不等于已验证硬件调码率或网络收益。

```powershell
$env:CARGO_TARGET_DIR = 'E:\desk-link\.worktrees\dl2-targets\windows-media'
python scripts/run-windows-cargo.py test -p desklink-windows --lib --test adaptive_bitrate --test configuration_queue --locked
python scripts/run-windows-cargo.py clippy -p desklink-windows --lib --tests --locked -- -D warnings
```

测试覆盖带宽阶跃规则、上下限、稀疏反馈、渲染压力、迟滞、手动门控、跨配置旧请求失效和5.1秒迟到原生确认。仍需真实带宽阶跃/丢包/弱硬件下测量文字清晰度、帧龄、恢复速度与切档频率；性能基线不可直接相减两台机器的系统时钟。
