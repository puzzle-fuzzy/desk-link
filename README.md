# DeskLink

DeskLink 是面向个人设备的跨平台远程桌面工具，目标是在自己的 Windows、Mac 和 iPhone 之间稳定查看并控制电脑桌面。

当前优先级：

1. Windows 桌面端；
2. macOS Apple Silicon 桌面端；
3. iOS 控制端。

项目设计资料位于 [`docs/`](docs/)，原始需求文档已原样归档。实现将采用 Rust 共享核心 + Windows 原生能力 + macOS 原生能力，iOS 在桌面端链路稳定后接入。

## 当前阶段

当前仓库处于设计确认阶段，暂不包含产品实现代码。已确认的设计规格位于：

[`docs/superpowers/specs/2026-07-15-desklink-design.md`](docs/superpowers/specs/2026-07-15-desklink-design.md)
