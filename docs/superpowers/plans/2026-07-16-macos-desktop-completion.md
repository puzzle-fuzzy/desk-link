# macOS Apple Silicon Desktop Completion Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在 Apple Silicon macOS 上完成 DeskLink 控制端与被控端的安全连接、桌面视频、鼠标键盘输入、审批、重连、权限提示、诊断和应用构建验收；iOS 暂缓，Windows/Linux 不在本轮验收范围。

**Architecture:** Rust `desklink-ffi` 增加可取消的 `HostRuntime` 和稳定 C ABI，继续统一承担 QUIC、Noise、配对、审批、加密、重连和协议边界。Swift 只实现 macOS 平台能力：Keychain、ScreenCaptureKit、VideoToolbox、Metal、CGEvent、SwiftUI 和应用生命周期。控制端继续复用现有 `ControllerRuntime`，并把环境变量入口升级为配对邀请与已保存主机流程。

**Tech Stack:** Rust 2024、Tokio、QUIC/Noise、C ABI、Swift 6、SwiftPM、SwiftUI、ScreenCaptureKit、VideoToolbox、Metal/MetalKit、CoreGraphics/CGEvent、Security/Keychain。

## Global Constraints

- 只构建和验证 `aarch64-apple-darwin` / Apple Silicon macOS，SwiftPM 平台最低 macOS 13。
- 不添加 iOS UI，不修改 Windows 主机实现，不以 Windows 构建作为本轮完成条件。
- Swift 不直接创建 QUIC socket，不实现 Noise 或业务协议解析；网络与安全行为进入 Rust runtime。
- Host 未获得本地批准前不得启动屏幕采集、发送 VideoConfig、发送视频帧或注入输入。
- 私钥、relay join secret、PairingInvite 不写入明文文件、日志或普通 UI 文案；长期秘密只进 Keychain。
- 每个任务先写失败测试或编译验证，再写最小实现；每个任务结束运行自己的窄验证并提交。
- C ABI 只使用固定宽度字段、显式长度和明确的回调生命周期；`destroy` 必须等待后台 worker 退出。
- 保留现有协议/会话/加密类型和错误语义，新增能力通过清晰的 host 模块和平台 adapter 接入。

## File Map

- `crates/desklink-ffi/src/host.rs`：HostRuntime 状态、命令和事件接口。
- `crates/desklink-ffi/src/host_worker.rs`：HostRuntime 的 QUIC/Noise/能力协商/视频发送/输入接收 worker。
- `crates/desklink-ffi/src/lib.rs`：导出 host 模块、C ABI handle、边界校验和生命周期函数。
- `crates/desklink-ffi/include/desklink.h`：Rust FFI 的公共 host ABI 声明。
- `apps/macos/Sources/DeskLinkC/include/desklink.h`：SwiftPM 使用的同步 ABI 声明。
- `crates/desklink-ffi/tests/host_runtime.rs`：fake media + 本机 relay 的 HostRuntime/ControllerRuntime 集成测试。
- `crates/desklink-ffi/tests/host_abi.rs`：host C ABI 空指针、固定长度、审批和销毁测试。
- `apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift`：Host ABI 调用、回调转 MainActor 和 host 生命周期。
- `apps/macos/Sources/DeskLinkApp/Bridge/HostIdentityStore.swift`：Keychain host identity 编码和读写。
- `apps/macos/Sources/DeskLinkApp/Bridge/TrustedControllerStore.swift`：Keychain trusted controller 列表。
- `apps/macos/Sources/DeskLinkApp/Bridge/SavedHostStore.swift`：Keychain 已批准 host 连接材料。
- `apps/macos/Sources/DeskLinkApp/Permissions/MacPermissions.swift`：屏幕录制和辅助功能权限状态。
- `apps/macos/Sources/DeskLinkApp/Capture/ScreenCaptureSource.swift`：ScreenCaptureKit 显示器捕获。
- `apps/macos/Sources/DeskLinkApp/Capture/MacH264Encoder.swift`：VideoToolbox 编码、SPS/PPS、AVCC → Annex B。
- `apps/macos/Sources/DeskLinkApp/Input/MacInputInjector.swift`：CGEvent 注入和 ReleaseAll。
- `apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift`：NSEvent 到 DesklinkInput 的键盘映射。
- `apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift`：AppKit first-responder 键盘/鼠标输入桥。
- `apps/macos/Sources/DeskLinkApp/Bridge/ControllerBridge.swift`：控制端连接材料、回调和已保存主机状态。
- `apps/macos/Sources/DeskLinkApp/Views/RolePickerView.swift`：角色选择。
- `apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift`：控制端邀请粘贴、保存主机和连接状态。
- `apps/macos/Sources/DeskLinkApp/Views/HostHomeView.swift`：主机权限、邀请和审批界面。
- `apps/macos/Sources/DeskLinkApp/Views/ApprovalView.swift`：默认拒绝的本地审批界面。
- `apps/macos/Sources/DeskLinkApp/Views/SessionView.swift`：画面、输入、关键帧和断开控制。
- `apps/macos/Sources/DeskLinkApp/Views/DiagnosticsView.swift`：脱敏诊断状态。
- `apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift`：角色路由和应用退出生命周期。
- `apps/macos/Sources/DeskLinkApp/Bridge/RustBridge.swift`：Task 5 迁移完成后删除，避免同时存在两套控制端生命周期。
- `apps/macos/Tests/DeskLinkAppTests/HostIdentityStoreTests.swift`：host identity/trusted controller 编解码测试。
- `apps/macos/Tests/DeskLinkAppTests/MacPermissionsTests.swift`：权限状态和文案映射测试。
- `apps/macos/Tests/DeskLinkAppTests/MacH264EncoderTests.swift`：编码输出辅助逻辑测试。
- `apps/macos/Tests/DeskLinkAppTests/MacInputInjectorTests.swift`：坐标、键盘、释放状态测试。
- `apps/macos/Tests/DeskLinkAppTests/ControllerBridgeTests.swift`：控制端状态和凭据不回显测试。
- `apps/macos/Info.plist`：macOS 权限说明和 bundle 元数据。
- `apps/macos/Package.swift`：ScreenCaptureKit、AppKit、ApplicationServices 等框架链接声明。
- `scripts/build-macos-arm64.sh`：Rust FFI、Swift arm64、bundle 检查和产物路径。

---

### Task 1: Repair the macOS build baseline and isolate VideoToolbox flags

**Files:**
- Modify: `apps/macos/Sources/DeskLinkApp/Video/H264Decoder.swift:14-205`
- Create: `apps/macos/Tests/DeskLinkAppTests/H264DecoderTests.swift`

**Interfaces:**
- Produces `H264Decoder.decodeFlags: VTDecodeFrameFlags` as an internal testable constant and keeps `configure(sequenceHeader:width:height:version:)`, `receive(accessUnit:frameID:version:)`, `reset()` unchanged for existing callers.

- [ ] **Step 1: Write the failing test**

Add a `@MainActor` test that verifies the decoder starts empty, reset clears all frame/config state, and the SDK-specific asynchronous flag helper is nonzero:

```swift
@MainActor
final class H264DecoderTests: XCTestCase {
    func testDecoderStartsAndResetsWithoutRetainingStreamState() {
        let decoder = H264Decoder()
        XCTAssertNil(decoder.latestPixelBuffer)
        XCTAssertEqual(decoder.lastFrameID, 0)
        XCTAssertEqual(decoder.configVersion, 0)
        decoder.reset()
        XCTAssertNil(decoder.latestPixelBuffer)
        XCTAssertEqual(decoder.lastFrameID, 0)
        XCTAssertEqual(decoder.configVersion, 0)
    }

    func testAsynchronousDecodeFlagsAreAvailableOnCurrentSDK() {
        XCTAssertNotEqual(H264Decoder.decodeFlags.rawValue, 0)
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd apps/macos && swift test --arch arm64`

Expected: compile failure at `H264Decoder.swift` because `.enableAsynchronousDecompression` is not available under the current Swift SDK.

- [ ] **Step 3: Implement the minimal build fix**

Expose the flag as an internal static property and use the current SDK case reported by the compiler:

```swift
@MainActor
final class H264Decoder {
    static let decodeFlags: VTDecodeFrameFlags = [._EnableAsynchronousDecompression]
    // existing properties and methods remain unchanged
}
```

Replace the call-site argument with `flags: Self.decodeFlags`. Add `ScreenCaptureKit`, `AppKit`, and `ApplicationServices` to the macOS target linker settings only when the new host adapters are introduced; do not add unrelated frameworks in this baseline task.

- [ ] **Step 4: Run test to verify it passes**

Run: `cd apps/macos && swift test --arch arm64`

Expected: existing Swift tests plus the two decoder tests pass.

- [ ] **Step 5: Commit**

```sh
git add apps/macos/Package.swift apps/macos/Sources/DeskLinkApp/Video/H264Decoder.swift apps/macos/Tests/DeskLinkAppTests/H264DecoderTests.swift
git commit -m "fix(macos): adapt VideoToolbox decoder to current SDK"
```

### Task 2: Build the Rust HostRuntime protocol and worker

**Files:**
- Create: `crates/desklink-ffi/src/host.rs`
- Create: `crates/desklink-ffi/src/host_worker.rs`
- Modify: `crates/desklink-ffi/src/lib.rs:18-25`
- Create: `crates/desklink-ffi/tests/host_runtime.rs`

**Interfaces:**
- Produces `HostRuntime`, `HostCommand`, `HostEvent`, `HostState`, `HostIdentity`, and `HostError` for the C ABI layer.
- `HostCommand` is the only path for Swift-originated media/control commands.
- `HostEvent::ApprovalRequested` contains `[u8; 16] device_id`, `[u8; 32] verify_key`, and a redacted fingerprint string; it never contains private key or relay secret data.
- The test file defines `HostTestFixture { relay_addr, host_identity, controller_identity, host_events, controller_events }` with `new()`, `start_host()`, `approve()`, `reject()`, `send_test_video()`, `next_event()`, `next_event_timeout()`, `controller_received_video()`, and `received_release_all()` methods used by the tests below.

- [ ] **Step 1: Write the failing protocol tests**

Add tests covering approval gating, command ordering, and release semantics:

```rust
#[tokio::test]
async fn host_does_not_publish_video_before_approval() {
    let fixture = HostFixture::new().await;
    let host = fixture.start_host().await;
    let request = fixture.next_event(&host).await;
    assert!(matches!(request, HostEvent::ApprovalRequested { .. }));
    assert!(fixture.next_event_timeout(&host, Duration::from_millis(50)).await.is_none());
}

#[tokio::test]
async fn approval_allows_video_and_reject_emits_release_all() {
    let fixture = HostFixture::new().await;
    let host = fixture.start_host().await;
    fixture.approve(&host).await;
    fixture.send_test_video(&host).await;
    assert!(fixture.controller_received_video().await);

    fixture.reject(&host).await;
    assert!(fixture.received_release_all().await);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p desklink-ffi --test host_runtime`

Expected: the test target fails to compile because `HostRuntime` and the fake relay fixture do not exist.

- [ ] **Step 3: Implement the HostRuntime command/event model**

Define the internal interfaces before adding the C ABI:

```rust
pub enum HostCommand {
    Approve { controller_device_id: [u8; 16], controller_verify_key: [u8; 32] },
    Reject,
    SendVideoConfig { stream_id: u64, version: u32, width: u16, height: u16, bytes: Vec<u8> },
    SendVideoAccessUnit { stream_id: u64, frame_id: u64, config_version: u32, bytes: Vec<u8> },
    SendCursor { stream_id: u64, bytes: Vec<u8> },
    RequestKeyframe,
    ReleaseAll,
    Stop,
}

pub enum HostEvent {
    State(HostState),
    ApprovalRequested { device_id: [u8; 16], verify_key: [u8; 32], fingerprint: String },
    Input(InputEvent),
    KeyframeRequested,
    ReleaseAll,
    Metrics(HostMetrics),
    Error(HostError),
}
```

Implement the worker as a Tokio task that joins as `DeviceRole::Host`, performs the existing Noise responder flow, pauses before capabilities until `Approve`/`Reject`, and then serializes outgoing video through the existing encrypted lanes. Use a bounded command channel. Reject media commands before approval and after stop with a stable `InvalidState` error. Always send `ReleaseAll` before a terminal event and join the worker in `HostRuntime::destroy`.

- [ ] **Step 4: Run tests to verify the worker passes**

Run: `cargo test -p desklink-ffi --test host_runtime`

Expected: approval gating, encrypted media delivery, input decode, keyframe request and release tests pass.

- [ ] **Step 5: Run the focused Rust checks**

Run: `cargo fmt --all -- --check && cargo test -p desklink-ffi`

Expected: formatting and all existing FFI tests pass without changing controller behavior.

- [ ] **Step 6: Commit**

```sh
git add crates/desklink-ffi/src/host.rs crates/desklink-ffi/src/host_worker.rs crates/desklink-ffi/src/lib.rs crates/desklink-ffi/tests/host_runtime.rs
git commit -m "feat(ffi): add macOS host runtime"
```

### Task 3: Expose and test the Host C ABI

**Files:**
- Modify: `crates/desklink-ffi/src/lib.rs:28-180, 330-780`
- Modify: `crates/desklink-ffi/include/desklink.h`
- Modify: `apps/macos/Sources/DeskLinkC/include/desklink.h`
- Create: `crates/desklink-ffi/tests/host_abi.rs`

**Interfaces:**
- Produces opaque `DesklinkHostHandle`, `DesklinkHostConfig`, `DesklinkHostEvent`, `DesklinkHostInput`, `DesklinkHostEventCallback`, and the host functions listed below.
- Swift receives only fixed-layout data and callback-scoped buffers.
- Also produces `DesklinkSavedHostMaterial` and `desklink_controller_copy_saved_host_material`, which copies the already-validated session ID, relay authentication, server name and host verify key into a caller-owned Keychain staging buffer after controller approval.

- [ ] **Step 1: Write the failing ABI tests**

Add C ABI tests for null arguments, invite length, approval, media submission, and destroy:

```rust
#[test]
fn host_abi_rejects_null_and_wrong_invite_lengths() {
    assert_eq!(unsafe { desklink_host_create(std::ptr::null(), None, std::ptr::null_mut()) }, DesklinkResult::InvalidArgument);
    let mut handle = std::ptr::null_mut();
    let config = valid_host_config();
    assert_eq!(unsafe { desklink_host_create(&config, Some(callback), &mut handle) }, DesklinkResult::Ok);
    assert_eq!(unsafe { desklink_host_start_from_invite(handle, invalid_invite_ptr(), 180) }, DesklinkResult::InvalidArgument);
    unsafe { desklink_host_destroy(handle) };
}

#[test]
fn host_abi_destroy_waits_for_worker_and_emits_release_all() {
    let handle = create_host_handle();
    assert_eq!(unsafe { desklink_host_release_all(handle) }, DesklinkResult::Ok);
    unsafe { desklink_host_destroy(handle) };
    assert!(callback_state().saw_release_all);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p desklink-ffi --test host_abi`

Expected: compile failure because the host structs and symbols are absent.

- [ ] **Step 3: Add the fixed-layout C declarations**

Add equivalent declarations to both header copies. The central functions use this shape:

```c
typedef struct {
    const char *relay_url;
    const char *server_name;
    uint8_t host_device_id[16];
    uint8_t host_secret_key[32];
    uint32_t log_level;
} DesklinkHostConfig;

typedef struct {
    uint8_t session_id[16];
    uint8_t relay_authentication[32];
    uint8_t host_verify_key[32];
    char server_name[256];
} DesklinkSavedHostMaterial;

DesklinkResult desklink_host_create(
    const DesklinkHostConfig *config,
    DesklinkHostEventCallback callback,
    void *callback_context,
    DesklinkHostHandle **out_handle);
DesklinkResult desklink_host_start_pairing(
    DesklinkHostHandle *handle,
    uint8_t *invite_out,
    size_t invite_capacity,
    size_t *invite_len_out,
    uint64_t *expires_at_unix_s_out);
DesklinkResult desklink_host_start_from_invite(
    DesklinkHostHandle *handle,
    const uint8_t *invite,
    size_t invite_len);
DesklinkResult desklink_host_approve(
    DesklinkHostHandle *handle,
    const uint8_t controller_device_id[16],
    const uint8_t controller_verify_key[32]);
DesklinkResult desklink_host_reject(DesklinkHostHandle *handle);
DesklinkResult desklink_host_send_video_config(
    DesklinkHostHandle *handle,
    uint64_t stream_id,
    uint32_t config_version,
    uint16_t width,
    uint16_t height,
    const uint8_t *bytes,
    size_t bytes_len);
DesklinkResult desklink_host_send_video_access_unit(
    DesklinkHostHandle *handle,
    uint64_t stream_id,
    uint64_t frame_id,
    uint32_t config_version,
    const uint8_t *bytes,
    size_t bytes_len);
DesklinkResult desklink_host_send_cursor(
    DesklinkHostHandle *handle,
    uint64_t stream_id,
    const uint8_t *bytes,
    size_t bytes_len);
DesklinkResult desklink_host_release_all(DesklinkHostHandle *handle);
DesklinkResult desklink_host_stop(DesklinkHostHandle *handle);
void desklink_host_destroy(DesklinkHostHandle *handle);
DesklinkResult desklink_controller_copy_saved_host_material(
    DesklinkHandle *handle,
    DesklinkSavedHostMaterial *out_material);
```

Use the existing maximum payload constants for every `bytes_len` check. `desklink_host_start_pairing` must require a caller buffer large enough for `PAIRING_INVITE_BYTES`; it must never silently truncate an invite.

- [ ] **Step 4: Implement C ABI ownership and callback copying**

Wrap `HostRuntime` in an opaque `DesklinkHostHandle`. Clone callback context only as an opaque pointer, copy event data into a worker-owned temporary before invoking the callback, and make `desklink_host_destroy` stop, release, join, clear the callback, and free the handle exactly once.

- [ ] **Step 5: Run ABI and Rust tests**

Run: `cargo test -p desklink-ffi --test host_abi && cargo test -p desklink-ffi`

Expected: all host ABI and existing controller/FFI tests pass.

- [ ] **Step 6: Commit**

```sh
git add crates/desklink-ffi/src/lib.rs crates/desklink-ffi/include/desklink.h apps/macos/Sources/DeskLinkC/include/desklink.h crates/desklink-ffi/tests/host_abi.rs
git commit -m "feat(ffi): expose host C ABI"
```

### Task 4: Implement macOS host identity, permissions, capture, encoding and input adapters

**Files:**
- Create: `apps/macos/Sources/DeskLinkApp/Bridge/HostIdentityStore.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Bridge/TrustedControllerStore.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Permissions/MacPermissions.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Capture/ScreenCaptureSource.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Capture/MacH264Encoder.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Input/MacInputInjector.swift`
- Create: `apps/macos/Tests/DeskLinkAppTests/HostIdentityStoreTests.swift`
- Create: `apps/macos/Tests/DeskLinkAppTests/MacPermissionsTests.swift`
- Create: `apps/macos/Tests/DeskLinkAppTests/MacH264EncoderTests.swift`
- Create: `apps/macos/Tests/DeskLinkAppTests/MacInputInjectorTests.swift`
- Modify: `apps/macos/Package.swift:17-31`
- Modify: `apps/macos/Info.plist`

**Interfaces:**
- `MacPermissions.snapshot() -> MacPermissionSnapshot` is pure at the model boundary and exposes screen recording/accessibility status plus actionable system settings URLs.
- `ScreenCaptureSource.start(displayID:streamID:configuration:onFrame:) async throws` and `stop() async` own `SCStream`.
- `MacH264Encoder.start(width:height:) throws`, `encode(pixelBuffer:frameID:)`, `requestKeyframe()`, `stop()` produce `EncodedVideoEvent` values.
- `MacInputInjector.inject(_:) throws` and `releaseAll()` own pressed-key/button state.
- `MacInputCommand` has the cases `.move(normalizedX: Float, normalizedY: Float)`, `.mouseButton(button: MouseButton, pressed: Bool)`, `.wheel(deltaX: Int32, deltaY: Int32)`, `.key(code: UInt32, pressed: Bool, modifiers: Modifiers)`, and `.unicode(String, modifiers: Modifiers)`.

- [ ] **Step 1: Write failing adapter tests**

Add pure tests for Keychain record layout, permission model mapping, Annex B encoding metadata, coordinate conversion, Unicode keyboard events and release ordering:

```swift
func testInputInjectorReleaseAllClearsEveryPressedKeyAndButton() throws {
    let backend = RecordingCGEventBackend()
    let injector = MacInputInjector(backend: backend)
    try injector.inject(.key(code: 0x24, pressed: true, modifiers: []))
    try injector.inject(.mouseButton(.left, pressed: true))
    injector.releaseAll()
    XCTAssertEqual(backend.releasedKeys, [0x24])
    XCTAssertEqual(backend.releasedButtons, [.left])
    XCTAssertTrue(injector.pressedInputs.isEmpty)
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd apps/macos && swift test --arch arm64`

Expected: compile failure because the adapter types do not exist.

- [ ] **Step 3: Implement Keychain and permission adapters**

Use generic-password Keychain records with versioned, fixed binary payloads. `HostIdentityStore` stores 16-byte device ID followed by 32-byte secret key. `TrustedControllerStore` stores a version byte, count, and repeated fixed records containing device ID, verify key, timestamps and display name. Return typed errors for malformed data, duplicate records and Keychain status codes.

`MacPermissions` must keep actual system calls behind an injectable provider so unit tests do not open System Settings. Production uses `CGPreflightScreenCaptureAccess`, `CGRequestScreenCaptureAccess`, and `AXIsProcessTrustedWithOptions`.

- [ ] **Step 4: Implement ScreenCaptureKit source**

Use `SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)` to choose the display whose frame contains the main menu-bar origin. Configure `SCStreamConfiguration` for BGRA, the selected display pixel dimensions, bounded queue depth, and a 30 FPS minimum frame interval. On stream errors call the host bridge stop callback; never block the capture output queue waiting for network transmission.

- [ ] **Step 5: Implement VideoToolbox encoder**

Create a `VTCompressionSession` with BGRA input conversion handled by VideoToolbox, 30 FPS, a bounded average bitrate, and a keyframe interval. In the output callback, extract SPS/PPS from the format description, convert AVCC NAL units to Annex B, classify keyframes, and publish `EncodedVideoEvent.configuration` before the first access unit of a config version. Use `kVTEncodeFrameOptionKey_ForceKeyFrame` for the next frame after `requestKeyframe()`.

- [ ] **Step 6: Implement CGEvent input**

Define a `CGEventBackend` protocol and production backend. Map normalized coordinates to the selected display’s global frame, inject mouse events and wheel deltas, use virtual-key events for ordinary keys, and call `keyboardSetUnicodeString` for non-ASCII text. Keep pressed keys/buttons in a set and release them in deterministic order on `releaseAll()`.

- [ ] **Step 7: Update package frameworks and permission metadata**

Link `ScreenCaptureKit`, `AppKit`, `ApplicationServices`, `CoreMedia`, `CoreVideo`, `VideoToolbox`, `Metal`, `MetalKit`, `Security`, and `CoreGraphics` in `Package.swift`. Add the screen capture usage description, accessibility guidance copy, bundle identifier, and arm64-compatible app metadata to `Info.plist`.

- [ ] **Step 8: Run the adapter tests**

Run: `cd apps/macos && swift test --arch arm64`

Expected: all existing and new pure adapter tests pass. Permission tests must not require actual permissions.

- [ ] **Step 9: Commit**

```sh
git add apps/macos/Package.swift apps/macos/Info.plist apps/macos/Sources/DeskLinkApp/Bridge/HostIdentityStore.swift apps/macos/Sources/DeskLinkApp/Bridge/TrustedControllerStore.swift apps/macos/Sources/DeskLinkApp/Permissions apps/macos/Sources/DeskLinkApp/Capture apps/macos/Sources/DeskLinkApp/Input apps/macos/Tests/DeskLinkAppTests
git commit -m "feat(macos): add host capture encoding and input adapters"
```

### Task 5: Complete the macOS controller bridge and session input

**Files:**
- Create: `apps/macos/Sources/DeskLinkApp/Bridge/ControllerBridge.swift`
- Delete: `apps/macos/Sources/DeskLinkApp/Bridge/RustBridge.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Bridge/SavedHostStore.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Input/KeyboardMapper.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Input/SessionInputView.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Bridge/SecureConnectionSettings.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Video/H264Decoder.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Video/MetalVideoView.swift`
- Create: `apps/macos/Tests/DeskLinkAppTests/ControllerBridgeTests.swift`
- Modify: `apps/macos/Tests/DeskLinkAppTests/InputMapperTests.swift`

**Interfaces:**
- `SavedHostStore.save(_:)`, `loadAll()`, and `remove(id:)` persist only approved connection material in Keychain.
- `ControllerBridge.connect(invite:)`, `connect(savedHost:)`, `requestKeyframe()`, `send(input:)`, `releaseAll()`, and `disconnect()` are the only controller-side UI commands.
- `KeyboardMapper.map(keyCode: UInt16, characters: String?, modifiers: NSEvent.ModifierFlags, isDown: Bool) -> [MacInputCommand]` returns key down/up plus Unicode commands without retaining the `NSEvent` object.

- [ ] **Step 1: Write failing controller tests**

Cover invitation parsing, saved host storage, state transitions, no-secret UI output, keyboard modifiers, Unicode and ReleaseAll on disconnect:

```swift
func testControllerErrorDoesNotExposeRelayAuthentication() {
    let bridge = ControllerBridge.testing(error: "relay authentication failed")
    XCTAssertFalse(bridge.userFacingError.contains("AUTH_KEY"))
    XCTAssertFalse(bridge.userFacingError.contains("relay secret"))
}

func testKeyboardMapperPreservesUnicodeAndModifierFlags() {
    XCTAssertEqual(KeyboardMapper.map(
        keyCode: 0x24,
        characters: "中",
        modifiers: [.command, .shift],
        isDown: true
    ), [
        .key(code: 0x24, pressed: true, modifiers: [.meta, .shift]),
        .unicode("中", modifiers: [.meta, .shift]),
    ])
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd apps/macos && swift test --arch arm64`

Expected: compile failure for the new bridge/store/keyboard types.

- [ ] **Step 3: Implement saved host storage and invite connection**

Move the environment-variable connection path behind `ControllerBridge` while retaining it as a test-only/development fallback. Decode the fixed invite through the existing Rust FFI function, use the controller identity from Keychain, and only save a host record after the runtime emits `Connected` following approval. Store relay auth and host verify key in Keychain, never in `UserDefaults`.

- [ ] **Step 4: Implement keyboard and AppKit input view**

Make `SessionInputView` an `NSViewRepresentable` that becomes first responder, forwards `keyDown`, `keyUp`, mouse movement, button, drag and scroll events to `ControllerBridge`, and calls `releaseAll()` from `resignFirstResponder`, `viewDidDisappear`, and `deinit`. Do not send pointer coordinates from aspect-fit letterbox regions.

- [ ] **Step 5: Finish decoder, cursor and Metal lifecycle**

Keep only the newest accepted frame for display, reject old stream/config/frame IDs, expose cursor events through a lightweight overlay model, and invalidate the VideoToolbox session before replacing it. `MetalVideoView` must clear its texture when the bridge disconnects and preserve aspect-fit geometry for Retina sizes.

- [ ] **Step 6: Run controller tests**

Run: `cd apps/macos && swift test --arch arm64`

Expected: controller bridge, Keychain store, input mapper, H.264 and geometry tests pass.

- [ ] **Step 7: Commit**

```sh
git add apps/macos/Sources/DeskLinkApp/Bridge apps/macos/Sources/DeskLinkApp/Input apps/macos/Sources/DeskLinkApp/Video apps/macos/Tests/DeskLinkAppTests
git commit -m "feat(macos): complete controller bridge and desktop input"
```

### Task 6: Wire host and controller roles into the SwiftUI application

**Files:**
- Modify: `apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Views/RolePickerView.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Views/ControllerHomeView.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Views/HostHomeView.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Views/ApprovalView.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Views/HomeView.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Views/ConnectView.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Views/SessionView.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Views/DiagnosticsView.swift`
- Create: `apps/macos/Sources/DeskLinkApp/Bridge/HostBridge.swift`
- Modify: `apps/macos/Sources/DeskLinkApp/Bridge/DeskLinkEvents.swift`

**Interfaces:**
- `RolePickerView` selects `.controller` or `.host` and keeps role state in the app scene.
- `HostBridge` exposes `state`, `permissions`, `pairingInvite`, `pendingApproval`, `metrics`, `start()`, `stop()`, `createInvite()`, `approve()`, `reject()`, and `revoke(controller:)`.
- Host and controller bridges translate native callbacks to `@MainActor` published state; SwiftUI never retains raw C pointers.

- [ ] **Step 1: Write failing view-model tests**

Test that the host starts in a safe idle state, missing permissions disable capture/input, approval defaults to reject, and disconnect clears active input state.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cd apps/macos && swift test --arch arm64`

Expected: compile failure for missing role/host bridge types and view-model assertions.

The failing test file defines these exact assertions before the implementation:

```swift
@MainActor
func testHostBridgeStartsSafeAndApprovalIsNotImplicit() {
    let bridge = HostBridge.testing(permissionSnapshot: .denied)
    XCTAssertEqual(bridge.state, .idle)
    XCTAssertFalse(bridge.canStartCapture)
    XCTAssertFalse(bridge.pendingApproval?.isApproved ?? false)
}
```

- [ ] **Step 3: Implement HostBridge lifecycle**

Create the host handle with Keychain identity and relay configuration, start pairing only on explicit user action, forward `ApprovalRequested` to the UI, call `desklink_host_approve` only after the user confirms the displayed identity, start capture only after the approval callback, and stop capture before calling `desklink_host_stop`.

- [ ] **Step 4: Implement role and host views**

The role picker must show two actions. ControllerHomeView must provide invite paste, saved hosts, connect/reconnect, verify-key display and safe errors. HostHomeView must show screen-recording/accessibility permission cards, create/copy/cancel invite, pending approval details, trust/revoke actions, and stop host. The UI must not display relay join secrets.

- [ ] **Step 5: Implement the session and diagnostics views**

Use `SessionInputView` over `MetalVideoView`, add keyframe and disconnect controls, show connected/reconnecting/recovering/frozen metrics, and call `releaseAll()` on every exit path. Diagnostics must show only redacted state, frame counts, dropped frames, stream/config IDs and categorized errors.

- [ ] **Step 6: Add app lifecycle cleanup**

Add an `NSApplicationDelegate` or scene-phase coordinator that stops capture, releases local input, stops host/controller runtimes and clears callback contexts on termination. Window dismissal must not leave a live runtime unless the host is explicitly configured to keep running; the first release keeps the host in the foreground app only.

- [ ] **Step 7: Run Swift tests**

Run: `cd apps/macos && swift test --arch arm64`

Expected: all Swift tests pass and no test requires real screen-recording or accessibility permission.

- [ ] **Step 8: Commit**

```sh
git add apps/macos/Sources/DeskLinkApp
git commit -m "feat(macos): add host and controller role UI"
```

### Task 7: Add Apple Silicon app packaging and focused integration checks

**Files:**
- Modify: `scripts/build-macos-arm64.sh`
- Modify: `apps/macos/Info.plist`
- Create: `scripts/verify-macos-runtime.sh`
- Modify: `README.md`
- Modify: `docs/windows-two-pc-setup.md` only if the shared invite format description needs correction for macOS wording

**Interfaces:**
- `scripts/build-macos-arm64.sh --check` builds the Rust FFI target, Swift release target, arm64 app bundle and validates executable architecture, bundle identifier and permission metadata.
- `scripts/verify-macos-runtime.sh` runs Rust focused tests, Swift tests and the local relay fake-media integration test without requiring system permissions.

- [ ] **Step 1: Write the failing packaging check**

Add shell assertions for the app bundle, executable architecture, `CFBundleIdentifier`, `NSScreenCaptureUsageDescription`, and `LSMinimumSystemVersion`:

```sh
test -x "$APP/Contents/MacOS/DeskLinkApp"
file "$APP/Contents/MacOS/DeskLinkApp" | grep -q 'arm64'
/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist" >/dev/null
/usr/libexec/PlistBuddy -c 'Print :NSScreenCaptureUsageDescription' "$APP/Contents/Info.plist" >/dev/null
/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$APP/Contents/Info.plist" | grep -q '13'
```

- [ ] **Step 2: Run the check to verify it fails**

Run: `./scripts/build-macos-arm64.sh --check`

Expected: the current Swift decoder failure or missing app bundle causes the check to fail before all assertions complete.

- [ ] **Step 3: Implement the release build and app bundle**

Build `desklink-ffi` with `--target aarch64-apple-darwin`, build Swift release with `--arch arm64`, copy the executable and `Info.plist` into `dist/macos/DeskLink.app/Contents`, and run the existing `PlistBuddy` checks. Keep the script deterministic and fail if the Rust library or Swift executable is not arm64.

- [ ] **Step 4: Add focused verification script**

Implement:

```sh
set -eu
cargo fmt --all -- --check
cargo test -p desklink-ffi
cargo test --manifest-path tests/end-to-end/Cargo.toml
(cd apps/macos && swift test --arch arm64)
./scripts/build-macos-arm64.sh --check
```

The script must not invoke Windows targets, Windows-specific tests, or iOS builds.

- [ ] **Step 5: Update macOS usage documentation**

Document role selection, the first-run Screen Recording and Accessibility permission flow, pairing invite handling, Keychain persistence, reconnect behavior, and the exact Apple Silicon build/verification commands. Remove stale statements that describe macOS only as an environment-variable controller skeleton.

- [ ] **Step 6: Run packaging and focused verification**

Run: `./scripts/verify-macos-runtime.sh`

Expected: formatting, FFI tests, end-to-end recovery tests, Swift arm64 tests and app bundle checks all pass.

- [ ] **Step 7: Commit**

```sh
git add scripts/build-macos-arm64.sh scripts/verify-macos-runtime.sh apps/macos/Info.plist README.md docs/windows-two-pc-setup.md
git commit -m "build(macos): verify Apple Silicon desktop app"
```

### Task 8: Perform permissioned manual acceptance and final review

**Files:**
- Modify: `docs/superpowers/plans/2026-07-16-macos-desktop-completion.md` to mark completed steps and record command results.
- Modify: `README.md` with only verified Apple Silicon results.

- [ ] **Step 1: Start the local relay and host**

Use the existing relay development command and launch the arm64 app. On the host role, grant Screen Recording and Accessibility permissions only when the app explains why they are needed.

- [ ] **Step 2: Verify first pairing**

Generate a one-time invite, paste it into the controller, confirm the host displays the controller device ID and fingerprint, reject once, then repeat and approve. Confirm no video or input event is emitted before approval.

- [ ] **Step 3: Verify desktop interaction**

Watch the screen continuously, move the pointer to all four visible video corners, click, drag, scroll vertically and horizontally, press modifier combinations, and enter Chinese text. Confirm aspect-fit letterbox regions do not move the remote pointer.

- [ ] **Step 4: Verify recovery and release**

Request a keyframe, temporarily interrupt the relay, restore it, close the session window during a pressed mouse/key state, and terminate the app. Confirm the session recovers when configured, old frames are not displayed, and the host releases every pressed key/button.

- [ ] **Step 5: Verify persistence and revocation**

Reconnect using the saved host record without re-pasting the invite, revoke the trusted controller on the host, and confirm the old controller cannot reconnect until a new invitation and approval.

- [ ] **Step 6: Run final commands**

Run:

```sh
git status --short --branch
git diff --check
./scripts/verify-macos-runtime.sh
```

Expected: clean working tree after the final documentation commit, no diff-check errors, and all Apple Silicon verification commands pass.

- [ ] **Step 7: Commit the acceptance record**

```sh
git add docs/superpowers/plans/2026-07-16-macos-desktop-completion.md README.md
git commit -m "docs: record macOS desktop acceptance"
```

## Final Verification Checklist

- [x] `cargo fmt --all -- --check` passes.
- [x] `cargo test -p desklink-ffi` passes with host and controller tests.
- [x] `cargo test --manifest-path tests/end-to-end/Cargo.toml` passes.
- [x] `cd apps/macos && swift test --arch arm64` passes.
- [x] `./scripts/build-macos-arm64.sh --check` produces and validates an arm64 app bundle.
- [x] Host approval blocks capture/video/input until explicit approval (automated coverage).
- [x] Screen Recording and Accessibility permissions have actionable UI (automated coverage).
- [x] Controller supports invite paste, Keychain persistence, reconnect and safe error text (automated coverage).
- [ ] VideoToolbox/Metal display, mouse, keyboard, Unicode and ReleaseAll work in manual acceptance; a second Mac and interactive macOS permission/desktop access were unavailable in this environment.
- [x] No iOS or Windows implementation is included in this delivery.

## Final verification record — 2026-07-16

- Completed the macOS Apple Silicon controller and host implementation; iOS, Windows, and Linux are outside this delivery.
- `cargo fmt --all -- --check`, `cargo clippy -p desklink-ffi --all-targets -- -D warnings`, `cargo test -p desklink-ffi`, and the local relay recovery tests pass.
- `cd apps/macos && swift test --arch arm64` passes with 29 tests; `./scripts/build-macos-arm64.sh --check` produces and validates an arm64-only `dist/macos/DeskLink.app`.
- `./scripts/verify-macos-runtime.sh` runs the macOS-scoped Rust, relay, Swift, and packaging checks without system permissions.
- Host approval now publishes the connected transition, host transport loss retries with bounded backoff, and initial host QUIC I/O is created inside the persistent worker runtime. Retina capture and encoder dimensions share one protocol-capped size, normalized input uses top-origin coordinates, and forced IDR frames retain the protocol keyframe flag.
- Two-Mac pairing, Screen Recording/Accessibility consent, Keychain UI inspection, reconnect, revocation, and physical desktop interaction were not executed in this environment and remain manual acceptance items.
