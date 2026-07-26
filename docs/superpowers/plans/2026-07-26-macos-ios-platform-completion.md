# macOS 与 iOS Apple 端完成 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Apple Silicon macOS 上完成可真实验收的控制端与被控端，并新增一个只能作为控制端的 iOS 客户端，使 iPhone 可以安全控制 Windows 或 macOS 设备。

**Architecture:** 保留现有 Rust `desklink-ffi` 作为 Apple 端唯一的网络、安全和协议边界；它继续负责 QUIC、Noise、配对、审批、重连、视频分片和 `ReleaseAll`。将现有 macOS controller 的 Keychain、ControllerBridge、H.264 解码和纯数据模型抽成 Apple shared core；macOS 继续实现 ScreenCaptureKit、VideoToolbox 编码、CGEvent 和桌面输入，iOS 只实现 VideoToolbox 解码、Metal 显示、触控/触控板输入、软键盘和二维码连接。iOS 不实现完整被控端，不使用 ReplayKit 伪装成系统级远程控制。

**Tech Stack:** Rust 2024、Tokio、QUIC/Noise、固定布局 C ABI、Swift 6、SwiftUI、Swift Package、Xcode、VideoToolbox、CoreVideo、Metal/MetalKit、Security/Keychain、ScreenCaptureKit、CoreGraphics/CGEvent、UIKit、AVFoundation。

## Global Constraints

- macOS 验收目标为 Apple Silicon `aarch64-apple-darwin`，最低 macOS 13；不把 Intel Mac 纳入本轮完成条件。
- iOS 首版最低 iOS 16.0，只提供控制端；不得增加“共享此设备”或暗示 iOS 可以控制其他 App 的被控端入口。
- Rust 继续拥有 QUIC、Noise、身份认证、配对、审批、协议解析、重连、视频恢复和 `ReleaseAll`；Swift 不创建 QUIC socket，不解析未经 Rust 验证的网络密文。
- C ABI 必须保持现有 macOS controller/host 调用兼容；新增 iOS 能力优先使用新增函数或版本化字段，不直接改变已有结构的字段顺序。
- iOS 首版只走中继视频路径；不要把 Windows 专用 DirectLan 探测扩展到 iOS，也不新增本地网络权限作为首版依赖。
- macOS 被控端在明确批准前不得开始屏幕采集、发送 VideoConfig、发送视频帧或注入输入；iOS 控制端在退后台、断线、视图销毁和应用终止时必须发送/触发 `ReleaseAll`。
- 私钥、relay authentication、完整 pairing invite 不进入日志、诊断、普通 UI 或 UserDefaults；长期材料只进入 Keychain。
- 不扩展本轮范围到音频、剪贴板、文件传输、多人控制、iOS 系统级被控、UAC 安全桌面或后台静默服务；已有共享协议类型保持兼容即可。
- 每个任务先增加失败测试或最小构建门禁，再实现；每个任务结束运行窄验证并提交一个可回滚 commit。
- 任何“可用”结论必须区分 Rust/Swift 自动测试、同机双实例验收、跨设备人工验收、签名/公证验证；未执行的设备验收不得写成已完成。

## Current Baseline and Architecture Gate

当前仓库已经有以下事实，不应重新设计或重复实现：

- `apps/macos` 已有 controller/host SwiftUI UI、Keychain、ScreenCaptureKit、VideoToolbox 编码/解码、Metal、CGEvent 和 macOS arm64 打包脚本。
- `crates/desklink-ffi` 已有 `ControllerRuntime`、`HostRuntime`、host C ABI、审批前视频阻断、重连和终止时 `ReleaseAll`。
- `./scripts/verify-macos-runtime.sh` 在本机通过：Rust FFI/host/controller 测试、端到端恢复测试、arm64 Swift 构建和 40 个 Swift 测试均通过；这只是自动基线，不等同于系统权限或双机验收。
- 当前仓库没有 `apps/ios`、Xcode iOS target、iOS Swift 源码或 iOS 构建门禁。
- 当前 `ControllerRuntime::connect` 默认向协议声明 `Platform::MacOS`；iOS 接入必须增加显式平台入口，否则 iOS 会被错误识别为 macOS 控制端。

平台边界必须锁定为：

| 被控端 | 控制端 | 本计划状态 |
| --- | --- | --- |
| Windows | macOS | 回归并真实验收 |
| macOS | Windows | 真实验收 |
| macOS | macOS | 同机双实例先验收，第二台 Mac 可用时补跨机 |
| Windows | iOS | iOS 首要验收链路 |
| macOS | iOS | iOS 第二验收链路 |
| iOS | 任意 | 不做完整被控端 |

Apple 的公开文档将 ReplayKit 定义为录制/广播应用画面，UIKit 的 `sendEvent` 只负责向当前应用的 responder 分发事件；它们不能替代 macOS `CGEvent` 的系统级输入注入。因此 iOS 只能控制远端 Windows/macOS，不能作为本项目的完整被控端。[ReplayKit](https://developer.apple.com/documentation/replaykit)、[UIApplication.sendEvent](https://developer.apple.com/documentation/uikit/uiapplication/sendevent(_:))

## File Map

### Rust protocol and FFI

- Modify: `crates/desklink-ffi/src/lib.rs` — 保存 controller platform、增加 `desklink_create_for_platform` 和 C enum 映射；保留 `desklink_create` 的 macOS 默认行为。
- Modify: `crates/desklink-ffi/src/worker.rs` — 将 platform 传递到 `ControllerRuntime::connect_for_platform`。
- Modify: `crates/desklink-ffi/include/desklink.h` — 增加 `DesklinkPlatform` 和新增函数声明，不改变已有 struct 的字段顺序。
- Modify: `apps/macos/Sources/DeskLinkC/include/desklink.h` — 与 Rust public header 字节级同步。
- Modify: `crates/desklink-ffi/tests/abi.rs` — 验证旧 macOS 创建函数和新的 iOS 创建函数。
- Create: `crates/desklink-ffi/tests/controller_platform.rs` — 通过 fake relay 验证 Hello/Capabilities 声明的 platform。

### Shared Apple Swift core

- Create: `apps/apple/Package.swift` — macOS/iOS 可编译的 `DeskLinkAppleCore` library 和测试 target。
- Create: `apps/apple/Sources/DeskLinkAppleCore/DeskLinkC/include/desklink.h` — shared Swift target 使用的 C header，来源保持与 Rust header 同步。
- Move/Modify: `apps/macos/Sources/DeskLinkApp/Bridge/ControllerBridge.swift` → `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift` — 注入 relay/platform/store，移除 AppKit 依赖。
- Move/Modify: `apps/macos/Sources/DeskLinkApp/Bridge/ControllerIdentityStore.swift` → `apps/apple/Sources/DeskLinkAppleCore/ControllerIdentityStore.swift` — Keychain identity 编解码和平台无关错误。
- Move/Modify: `apps/macos/Sources/DeskLinkApp/Bridge/SavedHostStore.swift` → `apps/apple/Sources/DeskLinkAppleCore/SavedHostStore.swift` — Keychain saved host 记录。
- Move/Modify: `apps/macos/Sources/DeskLinkApp/Bridge/DeskLinkEvents.swift` → `apps/apple/Sources/DeskLinkAppleCore/DeskLinkEvents.swift` — shared state/model/input types。
- Move: `apps/macos/Sources/DeskLinkApp/Video/H264AnnexB.swift` → `apps/apple/Sources/DeskLinkAppleCore/H264AnnexB.swift`。
- Move: `apps/macos/Sources/DeskLinkApp/Video/H264Decoder.swift` → `apps/apple/Sources/DeskLinkAppleCore/H264Decoder.swift`。
- Move: `apps/macos/Sources/DeskLinkApp/Video/VideoGeometry.swift` → `apps/apple/Sources/DeskLinkAppleCore/VideoGeometry.swift`。
- Create: `apps/apple/Sources/DeskLinkAppleCore/KeychainStore.swift` — 可注入的 Keychain client，生产实现调用 Security，测试实现使用内存。
- Move/Modify: `apps/macos/Sources/DeskLinkApp/Input/MacInputInjector.swift` → 保留 macOS adapter，仅把 `Modifiers`、`MouseButton`、`MacInputCommand` 等纯模型移到 shared core 并命名为 `RemoteInputCommand`。
- Create: `apps/apple/Tests/DeskLinkAppleCoreTests/ControllerBridgeTests.swift` — platform、saved host、回调代数和敏感字段不回显测试。
- Create: `apps/apple/Tests/DeskLinkAppleCoreTests/H264DecoderTests.swift` — shared decoder reset/config/frame freshness 测试。
- Create: `apps/apple/Tests/DeskLinkAppleCoreTests/H264AnnexBTests.swift` — parameter set 和 Annex B/AVCC 测试。
- Create: `apps/apple/Tests/DeskLinkAppleCoreTests/KeychainStoreTests.swift` — malformed record、覆盖、删除和 secret 不回显测试。

### macOS app and Apple build

- Modify: `apps/macos/Package.swift` — 依赖 `DeskLinkAppleCore`，移除已迁移的 shared source，保留 AppKit/ScreenCaptureKit/CGEvent/host adapter。
- Modify: `apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift` — 使用 shared controller，保留 macOS 生命周期和 host shutdown。
- Modify: `apps/macos/Sources/DeskLinkApp/Views/ConnectView.swift` — 只保留 macOS pasteboard/UI 适配，连接状态来自 shared core。
- Modify: `apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift` — 将 AppKit key code 映射为 shared `RemoteInputCommand`。
- Modify: `apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift` — 使用 shared input model，继续保证窗口失焦/销毁时 `ReleaseAll`。
- Create: `apps/macos/DeskLink.entitlements` — 统一的本地开发/签名 entitlement 文件。
- Modify: `apps/macos/Info.plist` — 补齐版本字段、权限说明和签名所需元数据。
- Modify: `scripts/build-macos-arm64.sh` — 支持未签名检查和通过 `APPLE_SIGNING_IDENTITY` 的可选签名检查。
- Modify: `scripts/verify-macos-runtime.sh` — 调整 shared package 路径，保留 macOS 专用门禁。
- Create: `docs/apple/macos-apple-silicon-acceptance.md` — 真实权限、双实例、跨设备、断线和撤销验收记录模板。

### iOS app

- Create: `apps/ios/DeskLinkIOS.xcodeproj` — iOS application target、unit/UI test target、shared package dependency、device/simulator schemes。
- Create: `apps/ios/DeskLinkIOS/DeskLinkIOSApp.swift` — SwiftUI app entry and scene lifecycle。
- Create: `apps/ios/DeskLinkIOS/Info.plist` — bundle metadata、camera usage description。
- Create: `apps/ios/DeskLinkIOS/Configuration/IOSRuntimeConfiguration.swift` — relay URL、server name、`Platform.ios` 和构建环境注入。
- Create: `apps/ios/DeskLinkIOS/Views/IOSConnectionHomeView.swift` — 粘贴/手动输入/最近设备/扫码入口。
- Create: `apps/ios/DeskLinkIOS/Views/IOSSavedHostsView.swift` — Keychain saved host 列表和重新连接。
- Create: `apps/ios/DeskLinkIOS/Views/IOSSessionView.swift` — 远程画面、状态、断开、关键帧、输入模式切换。
- Create: `apps/ios/DeskLinkIOS/Views/IOSMetalVideoView.swift` — `UIViewRepresentable`/`MTKView` 渲染 shared `CVPixelBuffer`。
- Create: `apps/ios/DeskLinkIOS/Input/IOSTouchInputView.swift` — 直接触控和触控板模式的手势状态机。
- Create: `apps/ios/DeskLinkIOS/Input/IOSKeyboardInput.swift` — `UIKeyInput`/`UITextView` committed text、删除、回车和特殊键。
- Create: `apps/ios/DeskLinkIOS/Input/IOSSpecialKeyBar.swift` — Esc/Tab/Ctrl/Option/Command/Shift/方向键按钮。
- Create: `apps/ios/DeskLinkIOS/Pairing/IOSQRCodeScanner.swift` — AVFoundation metadata QR scanner。
- Create: `apps/ios/DeskLinkIOS/Lifecycle/IOSSessionLifecycle.swift` — foreground/background、断开、恢复和 keyframe 请求策略。
- Create: `apps/ios/DeskLinkIOSTests/IOSInputMapperTests.swift` — 两种触控模式、边界和 ReleaseAll。
- Create: `apps/ios/DeskLinkIOSTests/IOSPairingTests.swift` — invite 粘贴/扫码、过期/非法输入和 UI 不回显 secret。
- Create: `apps/ios/DeskLinkIOSTests/IOSLifecycleTests.swift` — background suspend、foreground reconnect 和 stale frame 丢弃。
- Create: `apps/ios/DeskLinkIOSUITests/IOSConnectionFlowUITests.swift` — 连接页默认焦点、保存设备、session 状态和无 host 入口。

### iOS Rust packaging and docs

- Create: `scripts/build-apple-rust.sh` — 构建 `aarch64-apple-ios` 与 `aarch64-apple-ios-sim` 的 `libdesklink_ffi.a`，生成带 header 的 `DeskLinkFFI.xcframework`。
- Create: `scripts/verify-ios.sh` — Rust iOS targets、shared Swift tests、simulator build/test、静态库架构和 Info.plist 检查。
- Modify: `README.md` — 从“macOS 仅研究代码”更新为 Apple development scope，同时明确 iOS 仅控制端和未完成的实机/签名验收。
- Modify: `DESIGN.md` — 加入 shared Apple core、macOS host/controller、iOS controller 的边界，删除与新范围冲突的“Windows only”架构描述。
- Modify: `TODO.md` — 增加 Apple 端未完成的实机验收、签名/公证和 iOS 设备测试，不勾选未执行项目。
- Create: `docs/apple/ios-controller-development.md` — iOS 构建、扫码、Keychain、两种触控、前后台限制和设备验收说明。
- Create: `docs/apple/2026-07-26-apple-platform-acceptance.md` — 汇总命令输出、设备矩阵、已知限制和未验收项。

---

### Task 1: Add an explicit controller platform to the Rust FFI without breaking macOS

**Files:**
- Modify: `crates/desklink-ffi/src/lib.rs`
- Modify: `crates/desklink-ffi/src/worker.rs`
- Modify: `crates/desklink-ffi/include/desklink.h`
- Modify: `apps/macos/Sources/DeskLinkC/include/desklink.h`
- Modify: `crates/desklink-ffi/tests/abi.rs`
- Create: `crates/desklink-ffi/tests/controller_platform.rs`

**Interfaces:**
- Add `#[repr(u32)] pub enum DesklinkPlatform { MacOS = 1, IOS = 2 }` and the equivalent C enum.
- Add `pub unsafe extern "C" fn desklink_create_for_platform(config: *const DesklinkConfig, platform: DesklinkPlatform, callback: Option<DesklinkEventCallback>, context: *mut c_void, out_handle: *mut *mut DesklinkHandle) -> DesklinkResult`.
- Keep `desklink_create(...)` as a wrapper that calls `desklink_create_for_platform(..., DesklinkPlatform::MacOS, ...)`.
- Store the platform in `DesklinkRuntime` and pass it through `ControllerWorker::start` → `connect_controller` → `ControllerRuntime::connect_for_platform`.
- `desklink_host_create` remains macOS-host semantics and must not accept an iOS host platform.

- [ ] **Step 1: Write the failing platform-forwarding test**

Add a fake-relay controller test that creates the new iOS runtime and asserts the first encrypted `Hello` and `Capabilities` messages declare `Platform::IOS` while keeping `DeviceRole::Controller` and `Codec::H264`:

```rust
#[tokio::test]
async fn ios_controller_declares_ios_platform_to_the_host() {
    let fixture = ControllerPlatformFixture::new().await;
    let controller = fixture.connect_with_platform(DesklinkPlatform::IOS).await;
    let hello = fixture.next_control().await;
    let capabilities = fixture.next_control().await;
    assert!(matches!(hello, ControlMessage::Hello {
        platform: Platform::IOS,
        role: DeviceRole::Controller,
    }));
    assert!(matches!(capabilities, ControlMessage::Capabilities(value)
        if value.platform == Platform::IOS
            && value.role == DeviceRole::Controller
            && value.codecs == vec![Codec::H264]
            && value.h264_profiles == vec![H264Profile::Main]));
    controller.destroy();
}
```

- [ ] **Step 2: Run the focused test and verify it fails**

Run: `cargo test -p desklink-ffi --test controller_platform ios_controller_declares_ios_platform_to_the_host -- --exact`

Expected: compile failure because `DesklinkPlatform`, `desklink_create_for_platform` and the platform field do not exist.

- [ ] **Step 3: Implement the additive C ABI**

Use the following compatibility shape in both headers:

```c
typedef enum DesklinkPlatform {
    DESKLINK_PLATFORM_MACOS = 1,
    DESKLINK_PLATFORM_IOS = 2,
} DesklinkPlatform;

DesklinkResult desklink_create_for_platform(
    const DesklinkConfig *config,
    DesklinkPlatform platform,
    DesklinkEventCallback callback,
    void *context,
    DesklinkHandle **out_handle
);

/* Existing desklink_create remains the macOS-compatible wrapper. */
```

Reject unknown platform values with `DESKLINK_INVALID_ARGUMENT`; keep all current null-pointer, callback lifetime and destroy behavior unchanged.

- [ ] **Step 4: Run Rust checks**

Run: `cargo fmt --all -- --check && cargo test -p desklink-ffi`

Expected: existing ABI/host/controller tests and the new iOS platform forwarding test pass.

- [ ] **Step 5: Commit**

```sh
git add crates/desklink-ffi/src/lib.rs crates/desklink-ffi/src/worker.rs \
  crates/desklink-ffi/include/desklink.h apps/macos/Sources/DeskLinkC/include/desklink.h \
  crates/desklink-ffi/tests/abi.rs crates/desklink-ffi/tests/controller_platform.rs
git commit -m "feat(ffi): declare Apple controller platform"
```

### Task 2: Extract the macOS controller into an Apple shared core

**Files:**
- Create: `apps/apple/Package.swift`
- Create: `apps/apple/Sources/DeskLinkAppleCore/DeskLinkC/include/desklink.h`
- Move/Modify: `apps/macos/Sources/DeskLinkApp/Bridge/ControllerBridge.swift` → `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift`
- Move/Modify: `apps/macos/Sources/DeskLinkApp/Bridge/ControllerIdentityStore.swift` → `apps/apple/Sources/DeskLinkAppleCore/ControllerIdentityStore.swift`
- Move/Modify: `apps/macos/Sources/DeskLinkApp/Bridge/SavedHostStore.swift` → `apps/apple/Sources/DeskLinkAppleCore/SavedHostStore.swift`
- Move/Modify: `apps/macos/Sources/DeskLinkApp/Bridge/DeskLinkEvents.swift` → `apps/apple/Sources/DeskLinkAppleCore/DeskLinkEvents.swift`
- Move: `apps/macos/Sources/DeskLinkApp/Video/H264AnnexB.swift` → `apps/apple/Sources/DeskLinkAppleCore/H264AnnexB.swift`
- Move: `apps/macos/Sources/DeskLinkApp/Video/H264Decoder.swift` → `apps/apple/Sources/DeskLinkAppleCore/H264Decoder.swift`
- Move: `apps/macos/Sources/DeskLinkApp/Video/VideoGeometry.swift` → `apps/apple/Sources/DeskLinkAppleCore/VideoGeometry.swift`
- Create: `apps/apple/Sources/DeskLinkAppleCore/KeychainStore.swift`
- Create: `apps/apple/Tests/DeskLinkAppleCoreTests/ControllerBridgeTests.swift`
- Create: `apps/apple/Tests/DeskLinkAppleCoreTests/H264DecoderTests.swift`
- Create: `apps/apple/Tests/DeskLinkAppleCoreTests/H264AnnexBTests.swift`
- Create: `apps/apple/Tests/DeskLinkAppleCoreTests/KeychainStoreTests.swift`
- Modify: `apps/macos/Package.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Input/MacInputInjector.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift`

**Interfaces:**
- `DeskLinkAppleCore` exposes `ControllerBridge`, `ConnectionState`, `Metrics`, `SavedHost`, `ControllerIdentity`, `RemoteInputCommand`, `Modifiers`, `MouseButton`, `H264Decoder`, `H264AnnexB` and `VideoGeometry` to the macOS executable and the iOS app.
- `DeskLinkPlatform` is the Swift enum with `.macos` and `.ios` cases; its internal mapping is the only place that converts to the C `DesklinkPlatform` enum.
- `ControllerBridge.init(configuration:identityStore:savedHostStore:)` receives a `DeskLinkRuntimeConfiguration` containing `relayURL`, `relayServerName` and `platform` instead of reading `ProcessInfo` directly.
- `ControllerBridge.testing(configuration:state:)` is a test-only factory that exposes `activeRuntimeForTesting`, `releaseAllCallCountForTesting` and generation-controlled callback injection; these probes are unavailable to production targets.
- The production Keychain implementation conforms to `KeychainStore`; tests inject an in-memory implementation and never mutate the user login keychain.
- `ControllerBridge` uses `desklink_create_for_platform` when `platform == .ios`; macOS continues to use the same default behavior through the explicit `.macos` value.

- [ ] **Step 1: Add the shared package manifest and a compiling test target**

Create a package with macOS 13 and iOS 16 platform declarations, a `DeskLinkC` C target, a `DeskLinkAppleCore` library target, and a `DeskLinkAppleCoreTests` test target. Link only cross-platform frameworks in the shared target: Foundation, Security, CoreMedia, CoreVideo and VideoToolbox.

Run: `cd apps/apple && swift test --arch arm64`

Expected: the new package initially fails because no shared source has been moved; this is the baseline for the extraction task.

- [ ] **Step 2: Move pure controller/video code and make its public boundary explicit**

Move the files in the File Map, then replace AppKit-specific `MacInputCommand` with:

```swift
public enum RemoteInputCommand: Equatable, Sendable {
    case move(normalizedX: Float, normalizedY: Float)
    case mouseButton(MouseButton, pressed: Bool)
    case wheel(deltaX: Int32, deltaY: Int32)
    case key(code: UInt32, pressed: Bool, modifiers: Modifiers)
    case unicode(String, modifiers: Modifiers)
}
```

Keep `KeyboardMapper`, `SessionInputView`, `MacInputInjector`, all host code and all AppKit imports in the macOS target. The shared decoder must continue to use the current SDK-safe `VTDecodeFrameFlags(rawValue: 1 << 0)` value and reject stale stream/config/frame IDs.

- [ ] **Step 3: Inject Keychain and runtime configuration**

Add these testable boundaries:

```swift
public protocol KeychainStore: Sendable {
    func read(service: String, account: String) throws -> Data?
    func write(_ data: Data, service: String, account: String) throws
    func delete(service: String, account: String) throws
}

public struct DeskLinkRuntimeConfiguration: Sendable, Equatable {
    public let relayURL: String
    public let relayServerName: String
    public let platform: DeskLinkPlatform
}
```

Validate that empty relay/server strings and unknown platform values fail before creating a Rust handle; do not silently fall back to `127.0.0.1` in the iOS production configuration.

- [ ] **Step 4: Update macOS to consume the shared package**

Add the local package dependency to `apps/macos/Package.swift`, remove duplicate moved files from the executable target, and pass:

```swift
DeskLinkRuntimeConfiguration(
    relayURL: configuredRelayURL,
    relayServerName: configuredRelayServerName,
    platform: .macos
)
```

Update macOS tests to import `DeskLinkAppleCore` and keep host/CGEvent/UI tests in `DeskLinkAppTests`.

- [ ] **Step 5: Run both shared and macOS tests**

Run:

```sh
(cd apps/apple && swift test --arch arm64)
(cd apps/macos && swift test --arch arm64)
```

Expected: shared tests pass, macOS’s existing 40-test behavior remains green, and no host capture/permission code is linked into the shared target.

- [ ] **Step 6: Commit**

```sh
git add apps/apple apps/macos/Package.swift apps/macos/Sources/DeskLinkApp \
  apps/macos/Tests/DeskLinkAppTests
git commit -m "refactor(apple): share controller and video core"
```

### Task 3: Finish macOS Apple Silicon packaging and real host/controller acceptance

**Files:**
- Create: `apps/macos/DeskLink.entitlements`
- Modify: `apps/macos/Info.plist`
- Modify: `scripts/build-macos-arm64.sh`
- Modify: `scripts/verify-macos-runtime.sh`
- Create: `scripts/launch-macos-loopback.sh`
- Create: `docs/apple/macos-apple-silicon-acceptance.md`
- Modify: `apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Capture/ScreenCaptureSource.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Capture/MacH264Encoder.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Input/MacInputInjector.swift`

**Interfaces:**
- `scripts/build-macos-arm64.sh --check` remains offline and does not require a signing identity.
- `scripts/build-macos-arm64.sh --sign` requires `APPLE_SIGNING_IDENTITY` and verifies the resulting bundle with `codesign --verify --deep --strict`.
- `scripts/launch-macos-loopback.sh` starts two separately configured app instances against the local relay without changing source defaults.
- `HostBridge` remains the only owner of ScreenCaptureKit/VideoToolbox encoder/CGEvent lifecycle; every stop/revoke/permission-loss path completes capture stop, encoder stop, Rust stop and local `ReleaseAll` in that order.

- [ ] **Step 1: Add failing packaging assertions**

Extend the build check to assert the bundle has:

```sh
test "$(lipo -archs "$APP/Contents/MacOS/DeskLinkApp")" = 'arm64'
/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist" | grep -qx 'com.desklink.desktop'
/usr/libexec/PlistBuddy -c 'Print :NSScreenCaptureUsageDescription' "$APP/Contents/Info.plist" >/dev/null
/usr/libexec/PlistBuddy -c 'Print :NSAccessibilityUsageDescription' "$APP/Contents/Info.plist" >/dev/null
```

Run: `./scripts/build-macos-arm64.sh --check`

Expected: the new signing/entitlement assertions fail until the bundle metadata and script options are implemented.

- [ ] **Step 2: Implement deterministic local and signed bundle modes**

Keep the current arm64 Rust/Swift build, add `--sign`, and use:

```sh
if [ "$SIGN" -eq 1 ]; then
    test -n "${APPLE_SIGNING_IDENTITY:-}"
    codesign --force --options runtime --timestamp \
        --entitlements "$ROOT/apps/macos/DeskLink.entitlements" \
        --sign "$APPLE_SIGNING_IDENTITY" "$APP"
    codesign --verify --deep --strict "$APP"
fi
```

Do not claim notarization from local signing. Add a separate documented command for the user’s Apple Developer credentials only after the signed bundle is known to launch.

- [ ] **Step 3: Run the macOS automated gate**

Run: `./scripts/verify-macos-runtime.sh`

Expected: Rust FFI/host/controller tests, local relay recovery, shared/macOS Swift tests and arm64 bundle checks pass. Windows failures are outside this gate.

- [ ] **Step 4: Run same-Mac dual-instance acceptance**

Run: `./scripts/launch-macos-loopback.sh`, then verify in order:

1. Host instance requests Screen Recording and Accessibility permissions separately and shows actionable System Settings links.
2. Host creates a one-time invite; controller pastes it; host first rejects, then creates a new invite and approves.
3. No capture, VideoConfig, video frame or input injection occurs before approval.
4. The controller receives a live H.264 frame, maps all four visible corners correctly, clicks, drags, scrolls, sends modifiers and enters Chinese text.
5. Interrupt the relay, restore it, observe reconnect/recovery and request a keyframe.
6. Close the controller while a mouse button/key is down and terminate the host; verify all remote input is released.
7. Reconnect from the Keychain saved host record, revoke the trusted controller, and verify the old saved record cannot reconnect.

Record each result, macOS version, Xcode/Swift version, permission state, relay mode and unresolved limitation in `docs/apple/macos-apple-silicon-acceptance.md`; do not mark physical two-Mac acceptance as complete when only same-Mac loopback was executed.

- [ ] **Step 5: Run cross-platform acceptance when a Windows peer is available**

Verify both directions:

```text
macOS controller -> Windows host
Windows controller -> macOS host
```

Use the existing Windows acceptance binary for the Windows side and the macOS app bundle for the Mac side. Confirm H.264 Main negotiation, pointer coordinate orientation, keyboard mappings, reconnect and ReleaseAll.

- [ ] **Step 6: Commit**

```sh
git add apps/macos/DeskLink.entitlements apps/macos/Info.plist \
  scripts/build-macos-arm64.sh scripts/verify-macos-runtime.sh \
  scripts/launch-macos-loopback.sh apps/macos/Sources/DeskLinkApp \
  docs/apple/macos-apple-silicon-acceptance.md
git commit -m "build(macos): harden Apple Silicon acceptance bundle"
```

### Task 4: Build Rust and Swift for iOS device/simulator

**Files:**
- Create: `scripts/build-apple-rust.sh`
- Create: `scripts/verify-ios.sh`
- Create: `apps/ios/DeskLinkIOS.xcodeproj`
- Create: `apps/ios/DeskLinkIOS/DeskLinkIOSApp.swift`
- Create: `apps/ios/DeskLinkIOS/Info.plist`
- Create: `apps/ios/DeskLinkIOS/Configuration/IOSRuntimeConfiguration.swift`
- Create: `apps/ios/DeskLinkIOSTests/IOSBuildConfigurationTests.swift`

**Interfaces:**
- `build-apple-rust.sh` produces `dist/apple/DeskLinkFFI.xcframework` with `ios-arm64` and `ios-arm64-simulator` slices and the exact shared C header.
- `IOSRuntimeConfiguration.production` reads only app configuration/UserDefaults supplied by the app bundle or test harness; it never reads a developer machine environment variable in a release build.
- The Xcode app target links `DeskLinkAppleCore`, `DeskLinkFFI.xcframework`, Network-compatible Rust symbols and VideoToolbox/Metal/Security/UIKit/AVFoundation frameworks.

- [ ] **Step 1: Add the failing target/build check**

Run:

```sh
./scripts/build-apple-rust.sh
xcodebuild -project apps/ios/DeskLinkIOS.xcodeproj \
  -scheme DeskLinkIOS \
  -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO build
```

Expected: the Rust target or Xcode project fails before the targets, xcframework, app target or header copy exist.

- [ ] **Step 2: Add the required Rust targets and static library packaging**

Implement the deterministic build commands:

```sh
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --release -p desklink-ffi --target aarch64-apple-ios
MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --release -p desklink-ffi --target aarch64-apple-ios-sim
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libdesklink_ffi.a \
  -headers crates/desklink-ffi/include \
  -library target/aarch64-apple-ios-sim/release/libdesklink_ffi.a \
  -headers crates/desklink-ffi/include \
  -output dist/apple/DeskLinkFFI.xcframework
```

The script must fail on missing target, missing static library, header mismatch, or wrong `lipo -archs` output; it must not silently substitute the macOS library.

- [ ] **Step 3: Create the minimal iOS app target**

Configure the app target with iOS 16 deployment, arm64 device/simulator architectures, `CFBundleIdentifier` `com.desklink.ios`, `NSCameraUsageDescription` for QR scanning, and no host permission descriptions. Add `DeskLinkAppleCore` as a local Swift package dependency and wire `IOSRuntimeConfiguration(platform: .ios)` into the app entry.

- [ ] **Step 4: Run simulator build/test and static checks**

Run:

```sh
xcodebuild -project apps/ios/DeskLinkIOS.xcodeproj \
  -scheme DeskLinkIOS \
  -sdk iphonesimulator \
  -destination 'generic/platform=iOS Simulator' \
  CODE_SIGNING_ALLOWED=NO test
./scripts/verify-ios.sh
```

Expected: the simulator target builds without a signing identity, the shared tests run, the app links the simulator Rust slice, and no iOS build links AppKit, ScreenCaptureKit or CGEvent.

- [ ] **Step 5: Commit**

```sh
git add scripts/build-apple-rust.sh scripts/verify-ios.sh apps/ios
git commit -m "build(ios): add device and simulator application target"
```

### Task 5: Implement the iOS connection, pairing, Keychain and QR flow

**Files:**
- Create: `apps/ios/DeskLinkIOS/Views/IOSConnectionHomeView.swift`
- Create: `apps/ios/DeskLinkIOS/Views/IOSSavedHostsView.swift`
- Create: `apps/ios/DeskLinkIOS/Pairing/IOSQRCodeScanner.swift`
- Modify: `apps/ios/DeskLinkIOS/DeskLinkIOSApp.swift`
- Modify: `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift`
- Create: `apps/ios/DeskLinkIOSTests/IOSPairingTests.swift`
- Create: `apps/ios/DeskLinkIOSUITests/IOSConnectionFlowUITests.swift`

**Interfaces:**
- `IOSConnectionHomeView` exposes paste, manual invite entry, scan QR, recent saved hosts and a clear `ConnectionState` mapping.
- `IOSRootDestination` contains only `.connect`, `.savedHosts` and `.more`; `.shareThisDevice` is intentionally not a valid case.
- `IOSQRCodeScanner` returns a `Data` invite to the same validation path as paste; it never writes the raw invite to disk or Keychain.
- iOS shows “连接设备”“已保存设备”“更多”; it does not show “共享此设备”, host permissions, approval queue or macOS-only diagnostics.
- `SavedHostStore` saves only validated `SavedHost` material after Rust reports the authenticated controller connection; it never saves the one-time invite itself.

- [ ] **Step 1: Write pairing/UI tests**

Add tests for the same inputs used by both paste and QR:

```swift
func testInvalidInviteDoesNotStartRustRuntime() {
    let bridge = ControllerBridge.testing(configuration: .iosTest)
    bridge.connect(invite: Data("not-a-desklink-invite".utf8))
    XCTAssertEqual(bridge.state, .failed("The pairing invitation is invalid."))
    XCTAssertNil(bridge.activeRuntimeForTesting)
}

func testIOSNavigationDoesNotExposeHostMode() {
    XCTAssertFalse(IOSRootDestination.allCases.contains(.shareThisDevice))
}
```

- [ ] **Step 2: Implement paste/manual/QR input using one validation path**

Trim whitespace, Base64-decode the complete invite, require `DESKLINK_PAIRING_INVITE_BYTES`, then call `ControllerBridge.connect(invite:)`. Use `UIPasteboard.general` only for an explicit user action. The scanner must stop its capture session before returning a result and must reject non-DeskLink QR payloads without displaying raw bytes.

- [ ] **Step 3: Implement saved host and connection states**

Pass `.ios` through `desklink_create_for_platform`; show `idle`, `pairing`, `connecting`, `connected`, `reconnecting`, `recovering`, `frozen`, `failed` using the existing shared vocabulary. A saved-host failure must leave the record intact and expose “重新连接” and “删除记录” actions.

- [ ] **Step 4: Run iOS tests**

Run: `xcodebuild -project apps/ios/DeskLinkIOS.xcodeproj -scheme DeskLinkIOS -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO test`

Expected: pairing tests, Keychain fake tests, navigation/UI tests and shared decoder tests pass; no host control entry is present.

- [ ] **Step 5: Commit**

```sh
git add apps/ios apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift \
  apps/apple/Tests apps/ios/DeskLinkIOSTests apps/ios/DeskLinkIOSUITests
git commit -m "feat(ios): add secure controller pairing flow"
```

### Task 6: Implement iOS Metal video, direct touch, trackpad and keyboard

**Files:**
- Create: `apps/ios/DeskLinkIOS/Views/IOSSessionView.swift`
- Create: `apps/ios/DeskLinkIOS/Views/IOSMetalVideoView.swift`
- Create: `apps/ios/DeskLinkIOS/Input/IOSTouchInputView.swift`
- Create: `apps/ios/DeskLinkIOS/Input/IOSKeyboardInput.swift`
- Create: `apps/ios/DeskLinkIOS/Input/IOSSpecialKeyBar.swift`
- Modify: `apps/apple/Sources/DeskLinkAppleCore/DeskLinkEvents.swift`
- Modify: `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift`
- Create: `apps/ios/DeskLinkIOSTests/IOSInputMapperTests.swift`
- Create: `apps/ios/DeskLinkIOSTests/IOSSessionPresentationTests.swift`

**Interfaces:**
- `IOSMetalVideoView` displays only the newest shared `CVPixelBuffer` in an aspect-fit `MTKView`, with an explicit `visibleVideoRect` used by input mapping.
- `IOSTouchInputView` supports `.direct` and `.trackpad` modes; it emits only `RemoteInputCommand` and never calls Rust directly.
- `IOSTouchMapper` is the pure geometry/gesture helper used by the view and tests; `TestableIOSKeyboardInput` is the test double for the UIKit keyboard responder.
- `IOSKeyboardInput` turns committed Unicode text into `.unicode`, special keys into `.key`, and releases all active keys when it resigns first responder.
- `IOSSessionView` displays only the remote session, connection state, “请求关键帧” and “断开连接”; metrics remain in a diagnostic sheet.

- [ ] **Step 1: Write input and rendering tests**

Cover the concrete mapping rules:

```swift
func testDirectTouchMapsOnlyInsideAspectFitVideoRect() {
    let mapper = IOSTouchMapper(videoSize: CGSize(width: 1920, height: 1080), bounds: CGRect(x: 0, y: 0, width: 390, height: 844), mode: .direct)
    XCTAssertNil(mapper.command(for: CGPoint(x: 0, y: 0), phase: .moved))
    XCTAssertEqual(mapper.command(for: CGPoint(x: 195, y: 422), phase: .moved), .move(normalizedX: 0.5, normalizedY: 0.5))
}

func testTrackpadTwoFingerPanBecomesBoundedWheelInput() {
    let command = IOSTouchMapper.wheel(deltaX: 0, deltaY: -1_201)
    XCTAssertEqual(command, .wheel(deltaX: 0, deltaY: -1_200))
}

func testKeyboardResignReleasesPressedRemoteKeys() {
    let input = TestableIOSKeyboardInput()
    input.sendSpecialKey(.control, pressed: true)
    input.resign()
    XCTAssertEqual(input.releaseAllCallCount, 1)
}
```

- [ ] **Step 2: Implement the shared Metal presentation path**

Use `VTDecompressionSession` from `DeskLinkAppleCore.H264Decoder`, keep only the latest accepted frame, render through `CIImage`/Metal, and request one keyframe after the decoder’s bounded failure threshold. Recompute aspect-fit geometry on orientation and safe-area changes; never map a touch in a letterbox area to a remote coordinate.

- [ ] **Step 3: Implement direct touch mode**

Use one-finger movement for absolute pointer movement; a tap sends pointer move + left down + left up; a drag sends left down once, moves continuously, then left up; long press sends right-button down/up; all active buttons are released on cancellation, gesture failure, view disappearance or disconnect.

- [ ] **Step 4: Implement trackpad mode and special keys**

Use one-finger pan for relative pointer movement, tap for left click, long press for right click, and two-finger pan for bounded wheel deltas. Pinch changes only local zoom and does not send remote input. Add a visible special-key bar for Esc, Tab, Ctrl, Option/Alt, Command/Windows, Shift and arrow keys.

- [ ] **Step 5: Implement committed-text keyboard input**

Use a hidden/transparent `UIKeyInput`-compatible text responder to send committed Unicode text, delete and return. Physical hardware keyboard events use `UIKeyCommand` where available; do not infer a full AppKit key map on iOS. The keyboard responder must call shared `ControllerBridge.releaseAll()` when it resigns.

- [ ] **Step 6: Run focused and simulator tests**

Run:

```sh
(cd apps/apple && swift test --arch arm64)
xcodebuild -project apps/ios/DeskLinkIOS.xcodeproj -scheme DeskLinkIOS \
  -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO test
```

Expected: H.264 stale-frame tests, aspect-fit/letterbox tests, direct touch, trackpad, special-key and keyboard release tests pass.

- [ ] **Step 7: Commit**

```sh
git add apps/ios/DeskLinkIOS/Views apps/ios/DeskLinkIOS/Input \
  apps/apple/Sources/DeskLinkAppleCore apps/ios/DeskLinkIOSTests
git commit -m "feat(ios): add remote desktop touch and video session"
```

### Task 7: Add iOS foreground/background recovery and terminal safety

**Files:**
- Create: `apps/ios/DeskLinkIOS/Lifecycle/IOSSessionLifecycle.swift`
- Modify: `apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift`
- Modify: `apps/ios/DeskLinkIOS/DeskLinkIOSApp.swift`
- Modify: `apps/ios/DeskLinkIOS/Views/IOSSessionView.swift`
- Create: `apps/ios/DeskLinkIOSTests/IOSLifecycleTests.swift`
- Modify: `crates/desklink-ffi/tests/controller_runtime.rs`

**Interfaces:**
- Add `ControllerBridge.suspendForBackground()` that releases input, stops the active Rust handle and clears decoder output without deleting the saved host record.
- Add `ControllerBridge.resumeFromBackground()` that reconnects the saved host through a fresh Rust handle, waits for a new stream/config and requests a new keyframe; it never replays buffered old frames.
- `IOSSessionLifecycle` observes `scenePhase`; background transition is idempotent and foreground transition is debounced so duplicate notifications cannot create two workers.

- [ ] **Step 1: Write lifecycle tests**

Cover:

```swift
func testBackgroundReleasesInputAndClearsDisplayedFrame() async {
    let bridge = ControllerBridge.testing(configuration: .iosTest, state: .connected(streamID: 7))
    bridge.suspendForBackground()
    XCTAssertNil(bridge.latestPixelBuffer)
    XCTAssertEqual(bridge.releaseAllCallCountForTesting, 1)
}

func testForegroundDoesNotDisplayTheRetiredStream() {
    let bridge = ControllerBridge.testing(configuration: .iosTest)
    bridge.acceptsForTesting(streamID: 8, frameID: 2)
    bridge.acceptsForTesting(streamID: 7, frameID: 99)
    XCTAssertEqual(bridge.activeStreamIDForTesting, 8)
}
```

Add a Rust regression test that a fresh controller connection after a background suspension uses a new stream/config boundary and that `ReleaseAll` is delivered before the old worker closes.

- [ ] **Step 2: Implement idempotent suspension**

On background, stop accepting touch/keyboard commands, call `releaseAll`, invalidate the decoder, destroy the handle after the worker exits, and keep only the validated saved host record. Do not use an unbounded background task or pretend the Rust QUIC worker remains active indefinitely.

- [ ] **Step 3: Implement foreground reconnect**

On active scene phase, reconnect once from the saved host, show “正在恢复连接”, ignore old callbacks by generation, reset the decoder on the new stream, and request a keyframe after the first new VideoConfig. If the saved record is missing or expired, show the connection page without exposing secret material.

- [ ] **Step 4: Verify background behavior in simulator and device build**

Run:

```sh
xcodebuild -project apps/ios/DeskLinkIOS.xcodeproj -scheme DeskLinkIOS \
  -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO test
./scripts/verify-ios.sh
```

Expected: duplicate background/foreground notifications do not duplicate workers, stale frames are never shown after resume, and all terminal paths release input.

- [ ] **Step 5: Commit**

```sh
git add apps/ios/DeskLinkIOS/Lifecycle apps/ios/DeskLinkIOS/DeskLinkIOSApp.swift \
  apps/ios/DeskLinkIOS/Views/IOSSessionView.swift \
  apps/apple/Sources/DeskLinkAppleCore/ControllerBridge.swift \
  apps/ios/DeskLinkIOSTests crates/desklink-ffi/tests/controller_runtime.rs
git commit -m "fix(ios): recover controller sessions across app lifecycle"
```

### Task 8: Run Apple acceptance matrix, update scope documentation and close release gates

**Files:**
- Modify: `README.md`
- Modify: `DESIGN.md`
- Modify: `TODO.md`
- Create: `docs/apple/ios-controller-development.md`
- Create: `docs/apple/2026-07-26-apple-platform-acceptance.md`
- Modify: `docs/apple/macos-apple-silicon-acceptance.md`
- Modify: `scripts/verify-macos-runtime.sh`
- Modify: `scripts/verify-ios.sh`

**Interfaces:**
- `scripts/verify-macos-runtime.sh` is the macOS gate and never requires an iOS device.
- `scripts/verify-ios.sh` is the iOS gate and reports simulator success separately from device/signing success.
- Documentation must use “自动验证通过”“同机双实例通过”“跨设备人工通过”“未验收”四种明确状态，不把 historical records or simulator results into live-device evidence.

- [x] **Step 1: Run the complete automated Apple gate**

Run:

```sh
git diff --check
cargo fmt --all -- --check
cargo test --workspace
cargo test -p desklink-ffi
cargo test --manifest-path tests/end-to-end/Cargo.toml
./scripts/verify-macos-runtime.sh
./scripts/verify-ios.sh
```

Expected: all Apple-scoped automated checks pass; Windows-only failures are recorded separately and do not alter the Apple gate result.

- [ ] **Step 2: Execute the macOS device matrix**

Record evidence for:

```text
same Mac: host instance <-> controller instance
Mac controller -> Windows host
Windows controller -> Mac host
```

For each successful connection, verify first approval, continuous 1080p/30 FPS-or-lower latest-frame display, all pointer corners, click/drag/scroll, English and Chinese text, disconnect release, relay interruption recovery and trusted-controller revocation. Test Screen Recording and Accessibility permission removal while a session is active.

- [ ] **Step 3: Execute the iOS simulator/device matrix**

Simulator acceptance covers navigation, paste/manual invite, fake saved hosts, decoder, geometry, touch state machine, special keys and background state transitions. A signed physical iPhone acceptance must cover:

```text
iPhone controller -> Windows host over relay
iPhone controller -> macOS host over relay
Wi-Fi -> cellular transition
portrait -> landscape transition
foreground -> background -> foreground
Chinese keyboard final text
Bluetooth keyboard special keys
```

If no physical iPhone is attached, record these as unexecuted device gates; do not mark iOS production-ready from simulator output.

- [ ] **Step 4: Validate signing and artifact architecture**

Run the available local identity/device checks:

```sh
codesign --verify --deep --strict dist/macos/DeskLink.app
xcodebuild -project apps/ios/DeskLinkIOS.xcodeproj -scheme DeskLinkIOS \
  -sdk iphoneos -destination 'generic/platform=iOS' build
xcodebuild -project apps/ios/DeskLinkIOS.xcodeproj -scheme DeskLinkIOS \
  -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build
```

Record whether the iOS device build was signed with an Apple Development identity and installed on a real device; do not call an unsigned simulator build a release artifact.

- [x] **Step 5: Update product and architecture documentation**

Change the current “Windows only / macOS research code” wording only to the extent supported by the acceptance record. State explicitly:

- macOS is an Apple Silicon controller/host development target with automated and recorded manual gates;
- iOS is a controller-only target for Windows/macOS;
- iOS system-level host control is not supported;
- real-device, signing and notarization status remains visible and separate from code/test status.

- [x] **Step 6: Commit the acceptance record and docs**

```sh
git add README.md DESIGN.md TODO.md docs/apple scripts/verify-macos-runtime.sh scripts/verify-ios.sh
git commit -m "docs: record Apple platform acceptance boundaries"
```

## Final Acceptance Checklist

- [ ] Rust FFI preserves the existing macOS ABI and explicitly declares iOS controller platform.
- [ ] Shared Apple core builds for macOS and iOS without AppKit/ScreenCaptureKit/CGEvent imports.
- [ ] macOS arm64 automated gate, bundle architecture check and host/controller lifecycle tests pass.
- [ ] macOS same-Mac dual-instance pairing, permission, video, input, reconnect, revoke and ReleaseAll checks are recorded.
- [ ] macOS ↔ Windows cross-platform checks are recorded when a Windows peer is available.
- [ ] iOS device and simulator Rust static libraries are packaged into the correct xcframework slices.
- [ ] iOS paste/manual/QR pairing, Keychain saved hosts and controller-only navigation are implemented and tested.
- [ ] iOS H.264/Metal session, direct touch, trackpad, special keys, Unicode input and ReleaseAll are implemented and tested.
- [ ] iOS background/foreground reconnect never displays stale frames or duplicates workers.
- [ ] Simulator results, physical-device results, signing results and notarization results are reported as separate evidence classes.
- [ ] No iOS system-level host-control promise, ReplayKit workaround, private API, jailbreak dependency or unbounded background worker is introduced.

## Self-review

- Spec coverage: macOS host/controller, iOS controller-only boundary, shared Rust platform declaration, Apple shared Swift core, Keychain, H.264/Metal, touch/trackpad, keyboard, QR, lifecycle, signing, automated gates and physical acceptance each have a dedicated task.
- Placeholder scan: the plan contains no `TBD`, `TODO`, “implement later” or unspecified acceptance step; unavailable physical-device checks are explicitly recorded as unexecuted rather than left vague.
- Type consistency: `DesklinkPlatform`/`desklink_create_for_platform`, `DeskLinkRuntimeConfiguration`, `KeychainStore`, `RemoteInputCommand`, `suspendForBackground` and `resumeFromBackground` are used consistently across the task interfaces.
- Scope check: macOS and iOS are separate platform deliverables sharing a narrow Apple core; iOS host control, audio, transfer, clipboard and Windows implementation changes are explicitly excluded.

## Execution record

- [x] Tasks 1–7 implemented and committed on the execution branch.
- [x] Apple runtime gates, shared Swift tests, iOS unsigned device build, iOS simulator tests and the full Rust workspace test passed on 2026-07-26.
- [x] Product/architecture documentation and the separate Apple acceptance record were updated.
- [ ] Physical iPhone, macOS permission/loopback, cross-device, signing and notarization gates remain intentionally open until those environments are available.
