# 契约测试对账交接（2026-10-28）

37 个 `scripts/tests` 源码契约测试自 macOS parity 大合并（23ba00d）后失败，windows-ci 因此红。本轮已完成诊断与分类，未完成逐个修复。续作者按此清单执行。

## 已验证的结论

1. **抽查样本均为"测试锚点过时"，非生产行为退步**（如 `bridge.consume(mapped)` 断言：生产回调仍是 `acceptsCallback → captureAdmission.matches(receipt) → consume` 正确顺序，仅测试切片终点锚点 `private extension` 位置移动把目标代码切出范围）。
2. 失败分散在约 20 类不同断言（无聚类），需逐个把断言更新到 parity 合并后的新结构（双输入路径 CapturedInputContext、Session/Clipboard/AudioPcm/File 事件 14..17 等）。

## 两个特殊项（非锚点过时）

- `test_rust_is_the_source_of_both_headers`：**头文件含 parity 合并手工扩展**（`DESKLINK_SESSION_JSON_MAX_BYTES` 等 6 个 define + 事件枚举 14..17），`sync-ffi-header.py` 不生成它们所以判不同步。正确修法：**更新 sync 脚本**从 Rust 常量/枚举生成完整头文件。⚠️ 直接重跑 sync 脚本会抹掉手工扩展（破坏性，已试错并回滚）。
- `test_production_ffi_ingress_is_bounded...` 等 "unexpectedly found" 类：断言禁止的模式现在存在（`decoder.configure(`、`videoSize =`），需判断是生产引入了契约禁止的写法还是契约需放宽。

## 失败清单生成命令

```sh
python -m unittest discover -s scripts/tests -p "test_*.py" 2>&1 | grep -E "^(FAIL|ERROR):"
```

对账方法（已验证）：断言子串在**全文**存在 → 修测试切片锚点；不存在 → 对照新结构重写断言或判定行为退步。

## 其他在队工作

1. deploy-managed-relay.py 的 `ssh.copy`（Windows ssh 管道 dd 上传）不可靠，本次部署用手动 scp 绕过——需修复避免下次踩坑（relay 已于 2026-10-28 成功部署 24bd652 并通过外部探针）。
2. DL2-014 性能基线未动工。
