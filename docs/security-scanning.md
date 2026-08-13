# DeskLink 安全扫描门禁

安全扫描是发布前的独立门禁，不替代两台真实 Windows 电脑的连接、输入和传输验收。

## 自动扫描

`.github/workflows/security-scan.yml` 在 `main`、Pull Request、每周定时任务和手动触发时运行：

- CodeQL 分别分析 `javascript-typescript` 与 `rust`，使用不需要构建产物的分析模式，避免把 Linux/macOS 图形依赖误当成 Windows 发布条件。
- `cargo audit` 扫描锁文件；真实漏洞和 yanked 包会让门禁失败。

## 告警边界

当前 workspace 还会输出 GTK3/GLib 等 Tauri 非 Windows 目标的“未维护/unsound”提示。这些提示不会被本门禁静默忽略，也不会伪装成已修复；它们会保留在工作流日志中，并在 `TODO.md` 的跨平台后续项中跟踪。Windows 首发是否可发布，只由 Windows 构建、签名、双机验收和发布预检共同决定。

出现新的 `vulnerability`、yanked 包或 CodeQL 告警时，必须先定位依赖/代码路径并处理，再更新版本和候选物料；不能通过扩大忽略列表来“修复”门禁。
