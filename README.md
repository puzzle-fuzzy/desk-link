# DeskLink

DeskLink 是面向个人设备的跨平台远程桌面工具，目标是在自己的 Windows、Mac 和 iPhone 之间稳定查看并控制电脑桌面。

当前优先级：

1. Windows 桌面端；
2. macOS Apple Silicon 桌面端；
3. iOS 控制端。

项目设计资料位于 [`docs/`](docs/)，原始需求文档已原样归档。实现将采用 Rust 共享核心 + Windows 原生能力 + macOS 原生能力，iOS 在桌面端链路稳定后接入。

## 当前阶段

当前仓库已完成共享 Rust 核心、QUIC 中继、稳定 C ABI 和 macOS Apple Silicon 控制端骨架，并加入 Windows 原生能力边界与确定性回环验证器。已确认的设计规格位于：

[`docs/superpowers/specs/2026-07-15-desklink-design.md`](docs/superpowers/specs/2026-07-15-desklink-design.md)

## 已验证命令

```sh
cargo test --workspace
./scripts/verify.sh
cd apps/macos && swift test --arch arm64
cd ../.. && ./scripts/build-macos-arm64.sh --check
cargo check --manifest-path apps/windows/Cargo.toml --target x86_64-pc-windows-msvc
cargo test --manifest-path tests/end-to-end/Cargo.toml
```

macOS arm64 的 Swift 输入映射、Rust FFI 链接和 VideoToolbox/Metal 编译检查已在本机通过。Windows 的 `DXGI Desktop Duplication`、Media Foundation 编码和 `SendInput` 代码可交叉检查，但采集冒烟、设备驱动和最终 PE 构建仍需 Windows MSVC 机器执行：

```powershell
.\scripts\build-windows.ps1 -Configuration Release -CheckOnly
cargo test --manifest-path apps/windows/Cargo.toml --test capture_smoke -- --nocapture
```
