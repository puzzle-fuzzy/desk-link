# DeskLink 第一阶段 MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 构建 DeskLink 第一条可验证的 Windows 被控端到 macOS Apple Silicon 控制端垂直链路，包含共享 Rust 核心、视频帧协议、输入协议、端到端会话、QUIC 传输、Windows 屏幕采集与编码、macOS 硬件解码与 Metal 显示。

**Architecture:** Rust workspace 提供纯协议、视频队列、会话状态、加密和 QUIC 传输；平台代码通过明确的 adapter 边界接入。Windows 第一阶段实现被控端采集/编码/输入注入，macOS Apple Silicon 实现控制端原生 UI、VideoToolbox 解码、Metal 显示和本地鼠标键盘输入；中继只匹配会话并转发密文。

**Tech Stack:** Rust stable、Cargo workspace、Tokio、Quinn、Serde/Postcard、Snow Noise、Ed25519、ChaCha20-Poly1305、Swift 6、SwiftUI、VideoToolbox、Metal/MetalKit、ScreenCaptureKit、Windows API、D3D11、DXGI Desktop Duplication、Media Foundation。

## Global Constraints

- macOS 第一版只构建和验证 arm64-apple-macos，不提供 Intel 产物。
- Windows 和 macOS 桌面端优先；iOS 控制端不在本计划内，使用稳定的 C ABI 预留接入边界。
- 第一阶段只捕获一块主显示器，最高 1920×1080，默认 30FPS，最低自动降至 10FPS。
- 视频队列采用最新帧优先：采集到编码最多 2 帧、编码到网络最多 2 帧、网络到组帧最多 3 帧、解码到渲染最多 2 帧。
- 视频使用低延迟不可靠 Datagram；会话控制、输入和视频配置使用可靠有序通道。
- 任何旧帧不得覆盖已显示的新帧；重连后必须生成新的 stream_id。
- 第一版单用户、单被控端、单控制会话，首次连接必须由被控端手动接受。
- 不建立账号系统，不保存屏幕画面、键盘文本、剪贴板内容或会话密钥。
- 所有外部输入必须有长度、枚举值、帧分片数量和时间窗口限制。
- 当前迭代环境为 Windows；优先完成 Rust/Windows 可验证内容，macOS 原生编译、链接、打包和跨机验收延后到 Apple Silicon 环境执行。
- 每个任务都必须先写失败测试，再实现最小行为，再运行窄范围测试，最后提交独立 commit。

---

## Planned File Map

### Workspace and tooling

- Create: Cargo.toml — workspace 成员、公共依赖和版本约束。
- Create: rust-toolchain.toml — stable toolchain 和 Apple Silicon target。
- Create: .gitignore — Rust、SwiftPM、Xcode 和构建产物。
- Create: scripts/verify.sh — 格式化、检查、测试和文档扫描。
- Modify: README.md — 开发命令、当前阶段和平台限制。

### Shared Rust core

- Create: crates/desklink-protocol/src/lib.rs — 稳定协议类型和版本常量。
- Create: crates/desklink-protocol/src/codec.rs — 有界 Postcard 编解码。
- Create: crates/desklink-protocol/tests/round_trip.rs — 协议契约测试。
- Create: crates/desklink-video/src/queue.rs — 最新帧优先有界队列。
- Create: crates/desklink-video/src/packet.rs — H.264 分片、组帧和过期策略。
- Create: crates/desklink-video/src/lib.rs — 视频包公共接口。
- Create: crates/desklink-video/tests/ordering.rs — 旧帧、缺片和关键帧恢复测试。
- Create: crates/desklink-session/src/state.rs — 会话状态机和动作。
- Create: crates/desklink-session/src/input.rs — 输入序列、归一化坐标和 ReleaseAll。
- Create: crates/desklink-session/src/lib.rs — 会话公共接口。
- Create: crates/desklink-session/tests/state_machine.rs — 状态和输入测试。
- Create: crates/desklink-crypto/src/identity.rs — Ed25519 设备身份和签名。
- Create: crates/desklink-crypto/src/pairing.rs — 临时连接码和过期规则。
- Create: crates/desklink-crypto/src/noise.rs — Noise XX 会话握手和密文帧。
- Create: crates/desklink-crypto/src/lib.rs — 加密公共接口。
- Create: crates/desklink-crypto/tests/handshake.rs — 身份、握手和过期测试。
- Create: crates/desklink-transport/src/lib.rs — 传输事件和抽象接口。
- Create: crates/desklink-transport/src/quic.rs — Quinn 客户端通道。
- Create: crates/desklink-transport/tests/localhost.rs — 本机 QUIC 通道测试。
- Create: server/relay/Cargo.toml — 中继服务依赖。
- Create: server/relay/src/lib.rs — 会话映射和密文转发逻辑。
- Create: server/relay/src/main.rs — QUIC 监听和运行时启动。
- Create: server/relay/tests/session.rs — 匹配、过期、单控制端测试。
- Create: crates/desklink-ffi/include/desklink.h — Swift/Objective-C 稳定 C ABI。
- Create: crates/desklink-ffi/src/lib.rs — FFI handle、事件回调和内存边界。
- Create: crates/desklink-ffi/tests/abi.rs — FFI 创建/销毁和事件测试。

### macOS Apple Silicon controller

- Create: apps/macos/Package.swift — SwiftPM executable 和系统框架链接。
- Create: apps/macos/Info.plist — 应用名称、Bundle ID 和权限说明。
- Create: apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift — SwiftUI 应用入口。
- Create: apps/macos/Sources/DeskLinkApp/Bridge/RustBridge.swift — C ABI 封装。
- Create: apps/macos/Sources/DeskLinkApp/Bridge/DeskLinkEvents.swift — Rust 事件转 Swift 状态。
- Create: apps/macos/Sources/DeskLinkApp/Views/HomeView.swift — 设备状态和连接入口。
- Create: apps/macos/Sources/DeskLinkApp/Views/ConnectView.swift — 连接码输入和错误显示。
- Create: apps/macos/Sources/DeskLinkApp/Views/SessionView.swift — 远程会话窗口。
- Create: apps/macos/Sources/DeskLinkApp/Views/DiagnosticsView.swift — 开发诊断面板。
- Create: apps/macos/Sources/DeskLinkApp/Video/H264Decoder.swift — VideoToolbox 解码。
- Create: apps/macos/Sources/DeskLinkApp/Video/MetalVideoView.swift — CVPixelBuffer 到 Metal。
- Create: apps/macos/Sources/DeskLinkApp/Input/InputMapper.swift — 鼠标坐标和键盘事件。
- Create: apps/macos/Sources/DeskLinkC/include/desklink.h — SwiftPM C module header copied from the Rust ABI header.
- Create: apps/macos/Sources/DeskLinkC/module.modulemap — SwiftPM module declaration for DeskLinkC.
- Create: apps/macos/Tests/DeskLinkAppTests/InputMapperTests.swift — 坐标和修饰键测试。
- Create: scripts/build-macos-arm64.sh — Rust 静态库、Swift 可执行文件和 .app 打包。

### Windows host

- Create: apps/windows/Cargo.toml — Windows 应用 crate 和 windows-rs features。
- Create: apps/windows/src/main.rs — Windows host 启动和生命周期。
- Create: apps/windows/src/window.rs — 原生状态窗口和连接确认 UI。
- Create: apps/windows/src/capture.rs — D3D11/DXGI Desktop Duplication。
- Create: apps/windows/src/encoder.rs — Media Foundation H.264 编码。
- Create: apps/windows/src/input.rs — SendInput 和 ReleaseAll。
- Create: apps/windows/tests/capture_smoke.rs — Windows 机器上的采集冒烟测试。
- Create: scripts/build-windows.ps1 — Windows 构建、测试和产物检查。

### End-to-end and documentation

- Create: tests/end-to-end/src/main.rs — 合成视频源到控制端的回环验证器。
- Create: tests/end-to-end/Cargo.toml — 回环验证器依赖。
- Modify: README.md — 实际构建和验证命令。
- Modify: docs/superpowers/specs/2026-07-15-desklink-design.md — 实际验证结果。

---

## Task 1: 建立可验证的 Rust/Swift 工作区

**Files:**

- Create: Cargo.toml
- Create: rust-toolchain.toml
- Create: .gitignore
- Create: crates/*/Cargo.toml and src/lib.rs for six shared crates
- Create: server/relay/Cargo.toml and src/lib.rs
- Create: scripts/verify.sh
- Modify: README.md

**Interfaces:**

- Produces workspace packages named desklink-protocol, desklink-video, desklink-session, desklink-crypto, desklink-transport, desklink-ffi and desklink-relay.
- Later tasks consume the workspace dependency aliases from the root manifest.

- [ ] Step 1: Run the failing workspace smoke command

~~~bash
cargo metadata --no-deps --format-version 1
~~~

Expected: FAIL because Cargo.toml does not exist.

- [ ] Step 2: Create the workspace manifests

~~~toml
[workspace]
resolver = "2"
members = [
  "crates/desklink-protocol",
  "crates/desklink-video",
  "crates/desklink-session",
  "crates/desklink-crypto",
  "crates/desklink-transport",
  "crates/desklink-ffi",
  "server/relay",
]

[workspace.package]
edition = "2024"
rust-version = "1.85"
license = "MIT"

[workspace.dependencies]
desklink-protocol = { path = "crates/desklink-protocol" }
desklink-video = { path = "crates/desklink-video" }
desklink-session = { path = "crates/desklink-session" }
desklink-crypto = { path = "crates/desklink-crypto" }
desklink-transport = { path = "crates/desklink-transport" }
serde = { version = "1", features = ["derive"] }
thiserror = "2"
~~~

Each package starts with a src/lib.rs exporting one public PACKAGE_NAME constant, so the empty workspace has a real compilation target.

- [ ] Step 3: Add toolchain and ignore rules

Create rust-toolchain.toml:

~~~toml
[toolchain]
channel = "stable"
components = ["rustfmt", "clippy"]
targets = ["aarch64-apple-darwin", "x86_64-pc-windows-msvc"]
~~~

Ignore target/, .build/, .swiftpm/, DerivedData/, .DS_Store, .idea/ and .vscode/.

- [ ] Step 4: Run the baseline checks

~~~bash
cargo metadata --no-deps --format-version 1
cargo fmt --all -- --check
cargo test --workspace
~~~

Expected: metadata lists seven Rust packages, formatting passes, and all package smoke tests pass.

- [ ] Step 5: Add scripts/verify.sh

~~~sh
#!/bin/sh
set -eu
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
marker_a=$(printf '\\u5f85\\u5b9a')
marker_b=$(printf '\\u5f85\\u8865\\u5145')
if rg -n "T[O][D][O]|T[B][D]|$marker_a|$marker_b" README.md docs crates server tests; then
  echo 'placeholder text found' >&2
  exit 1
fi
~~~

- [ ] Step 6: Commit the workspace baseline

~~~bash
git add Cargo.toml rust-toolchain.toml .gitignore crates server scripts/verify.sh README.md
git commit -m "build: establish DeskLink workspace"
~~~

## Task 2: 定义协议类型与有界编解码

**Files:**

- Create: crates/desklink-protocol/src/lib.rs
- Create: crates/desklink-protocol/src/codec.rs
- Create: crates/desklink-protocol/tests/round_trip.rs
- Modify: crates/desklink-protocol/Cargo.toml

**Interfaces:**

- Produces ControlMessage, VideoFrameHeader, VideoPacket, InputEvent, DeviceCapabilities, ErrorCode, Platform, DeviceRole and Codec.
- Produces encode_control, decode_control, encode_video_header and decode_video_header.
- Later tasks use these types instead of defining duplicate protocol enums.

- [ ] Step 1: Write failing protocol tests

~~~rust
#[test]
fn control_message_round_trips() {
    let message = ControlMessage::RequestKeyframe { stream_id: 7 };
    let encoded = encode_control(&message).expect("encode");
    assert_eq!(decode_control(&encoded).expect("decode"), message);
}

#[test]
fn frame_header_round_trips() {
    let header = VideoFrameHeader {
        protocol_version: PROTOCOL_VERSION,
        stream_id: 3,
        config_version: 2,
        frame_id: 41,
        capture_timestamp_us: 1234,
        width: 1920,
        height: 1080,
        flags: FrameFlags::KEYFRAME,
        chunk_index: 0,
        chunk_count: 2,
        payload_length: 900,
    };
    let encoded = encode_video_header(&header).expect("encode");
    assert_eq!(decode_video_header(&encoded).expect("decode"), header);
}

#[test]
fn oversized_control_payload_is_rejected() {
    let bytes = vec![0u8; MAX_CONTROL_MESSAGE_BYTES + 1];
    assert!(matches!(decode_control(&bytes), Err(ProtocolError::MessageTooLarge { .. })));
}
~~~

- [ ] Step 2: Run focused tests and verify failure

~~~bash
cargo test -p desklink-protocol --test round_trip
~~~

Expected: FAIL because the protocol types and codec functions are not defined.

- [ ] Step 3: Implement the stable protocol model

Use these public shapes:

~~~rust
pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Platform { Windows, MacOS, IOS }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeviceRole { Controller, Host }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Codec { H264 }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameFlags(pub u16);

impl FrameFlags {
    pub const KEYFRAME: Self = Self(1 << 0);
    pub const CONFIG: Self = Self(1 << 1);
    pub const VIDEO_ALIVE: Self = Self(1 << 2);
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VideoFrameHeader {
    pub protocol_version: u16,
    pub stream_id: u64,
    pub config_version: u32,
    pub frame_id: u64,
    pub capture_timestamp_us: u64,
    pub width: u16,
    pub height: u16,
    pub flags: FrameFlags,
    pub chunk_index: u16,
    pub chunk_count: u16,
    pub payload_length: u32,
}
~~~

The model also defines ControlMessage, DeviceCapabilities, MouseButton, KeyCode, Modifiers and InputEvent. Reject protocol versions other than PROTOCOL_VERSION, zero chunk_count, chunk_index >= chunk_count, dimensions above 3840×2160, and payloads above 1200 bytes.

- [ ] Step 4: Implement bounded Postcard codec

~~~rust
pub fn encode_control(message: &ControlMessage) -> Result<Vec<u8>, ProtocolError>;
pub fn decode_control(bytes: &[u8]) -> Result<ControlMessage, ProtocolError>;
pub fn encode_video_header(header: &VideoFrameHeader) -> Result<Vec<u8>, ProtocolError>;
pub fn decode_video_header(bytes: &[u8]) -> Result<VideoFrameHeader, ProtocolError>;
~~~

decode_* checks byte length before calling Postcard and maps decode failures to ProtocolError::Malformed. No decoder allocates based on an untrusted length before the cap is checked.

- [ ] Step 5: Run focused and workspace tests

~~~bash
cargo test -p desklink-protocol --test round_trip
cargo test --workspace
~~~

Expected: all protocol tests pass.

- [ ] Step 6: Commit the protocol contract

~~~bash
git add crates/desklink-protocol
git commit -m "feat(protocol): define versioned remote desktop messages"
~~~

## Task 3: 实现最新帧优先的视频队列和组帧器

**Files:**

- Create: crates/desklink-video/src/queue.rs
- Create: crates/desklink-video/src/packet.rs
- Create: crates/desklink-video/src/lib.rs
- Create: crates/desklink-video/tests/ordering.rs
- Modify: crates/desklink-video/Cargo.toml

**Interfaces:**

- Produces LatestFrameQueue<T>, FrameAssembler, EncodedFrame, DropReason and AssembleResult.
- LatestFrameQueue::push_latest keeps the newest item and reports the evicted item.
- FrameAssembler::push, expire and accept_for_present are the only paths for packet ordering.

- [ ] Step 1: Write failing queue and frame tests

~~~rust
#[test]
fn queue_evicts_oldest_when_full() {
    let mut queue = LatestFrameQueue::new(2);
    queue.push_latest(1);
    queue.push_latest(2);
    assert_eq!(queue.push_latest(3), Some(1));
    assert_eq!(queue.drain_newest_first(), vec![3, 2]);
}

#[test]
fn incomplete_frame_expires_without_blocking_new_frame() {
    let mut assembler = FrameAssembler::new(3, Duration::from_millis(120));
    assert_eq!(assembler.push(instant(0), packet(10, 0, 2)), AssembleResult::Pending);
    assert_eq!(assembler.push(instant(121), packet(11, 0, 1)), AssembleResult::Complete(frame(11)));
    assert_eq!(assembler.expire(instant(121)), 1);
}

#[test]
fn older_frame_cannot_be_presented() {
    let mut assembler = FrameAssembler::new(3, Duration::from_millis(120));
    assert!(assembler.accept_for_present(frame(20)));
    assert!(!assembler.accept_for_present(frame(19)));
}
~~~

- [ ] Step 2: Run focused tests and verify failure

~~~bash
cargo test -p desklink-video --test ordering
~~~

Expected: FAIL because the queue and assembler interfaces do not exist.

- [ ] Step 3: Implement the bounded newest-first queue

~~~rust
pub struct LatestFrameQueue<T> {
    capacity: usize,
    items: VecDeque<T>,
}

impl<T> LatestFrameQueue<T> {
    pub fn new(capacity: usize) -> Self;
    pub fn push_latest(&mut self, item: T) -> Option<T>;
    pub fn pop_newest(&mut self) -> Option<T>;
    pub fn len(&self) -> usize;
}
~~~

Reject capacity zero. The pipeline creates queues with capacities 2, 2, 3 and 2 at the four documented boundaries.

- [ ] Step 4: Implement frame packet validation and assembly

~~~rust
pub struct FrameAssembler {
    max_frames: usize,
    max_age: Duration,
    frames: BTreeMap<(u64, u64), PartialFrame>,
    last_presented: Option<(u64, u64)>,
}

impl FrameAssembler {
    pub fn new(max_frames: usize, max_age: Duration) -> Self;
    pub fn push(&mut self, now: Instant, packet: VideoPacket) -> AssembleResult;
    pub fn expire(&mut self, now: Instant) -> usize;
    pub fn accept_for_present(&mut self, frame: EncodedFrame) -> bool;
}
~~~

push rejects malformed headers, duplicate chunks and mismatched frame metadata. It returns Complete only after every chunk arrives, evicts the oldest incomplete frame when max_frames is reached, and accept_for_present rejects all stale frames.

- [ ] Step 5: Run focused and workspace tests

~~~bash
cargo test -p desklink-video --test ordering
cargo test --workspace
~~~

Expected: queue, timeout, stale-frame and regression tests pass.

- [ ] Step 6: Commit the video pipeline

~~~bash
git add crates/desklink-video
git commit -m "feat(video): add bounded newest-frame assembly"
~~~

## Task 4: 实现输入规范化和会话状态机

**Files:**

- Create: crates/desklink-session/src/input.rs
- Create: crates/desklink-session/src/state.rs
- Create: crates/desklink-session/src/lib.rs
- Create: crates/desklink-session/tests/state_machine.rs
- Modify: crates/desklink-session/Cargo.toml

**Interfaces:**

- Produces NormalizedPoint, map_to_desktop, InputSequencer, PressedInputState, SessionMachine, SessionEvent and SessionAction.
- SessionMachine::apply produces explicit actions and never leaves an unrecognized error in a connecting state.
- PressedInputState::release_all returns the exact events needed to clear modifiers and pointer buttons.

- [ ] Step 1: Write failing input and state tests

~~~rust
#[test]
fn normalized_point_maps_to_desktop_origin_and_end() {
    assert_eq!(
        map_to_desktop(NormalizedPoint::new(0.0, 0.0), DesktopRect::new(-100, 20, 1920, 1080)),
        (-100, 20)
    );
    assert_eq!(
        map_to_desktop(NormalizedPoint::new(1.0, 1.0), DesktopRect::new(-100, 20, 1920, 1080)),
        (1820, 1100)
    );
}

#[test]
fn disconnect_emits_release_all() {
    let mut machine = SessionMachine::new(DeviceRole::Controller);
    machine.apply(SessionEvent::RelayConnected).unwrap();
    machine.apply(SessionEvent::HandshakeComplete).unwrap();
    let actions = machine.apply(SessionEvent::Disconnected { retryable: true }).unwrap();
    assert!(actions.contains(&SessionAction::ReleaseAll));
    assert_eq!(machine.state(), SessionState::Reconnecting);
}

#[test]
fn approval_is_required_before_video_start() {
    let mut machine = SessionMachine::new(DeviceRole::Host);
    machine.apply(SessionEvent::RelayConnected).unwrap();
    machine.apply(SessionEvent::HandshakeComplete).unwrap();
    assert_eq!(machine.state(), SessionState::WaitingForApproval);
    assert!(machine.apply(SessionEvent::StartVideo).is_err());
}
~~~

- [ ] Step 2: Run focused tests and verify failure

~~~bash
cargo test -p desklink-session --test state_machine
~~~

Expected: FAIL because coordinate mapping and state machine types are not defined.

- [ ] Step 3: Implement coordinate mapping and input sequence tracking

~~~rust
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizedPoint { pub x: f32, pub y: f32 }

impl NormalizedPoint {
    pub fn new(x: f32, y: f32) -> Self;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopRect { pub left: i32, pub top: i32, pub width: u32, pub height: u32 }

pub fn map_to_desktop(point: NormalizedPoint, desktop: DesktopRect) -> (i32, i32);
pub fn next_input_sequence(sequence: &mut u64) -> u64;
~~~

Clamp coordinates to 0.0..=1.0, use floor for pixel conversion, and increment sequences with wrapping arithmetic while reserving 0 as unused initial value.

- [ ] Step 4: Implement session states and release behavior

~~~rust
pub enum SessionState {
    Idle, CreatingSession, ConnectingRelay, SecureHandshake,
    WaitingForApproval, NegotiatingCapabilities, StartingVideo,
    Connected, Degraded, RecoveringVideo, Reconnecting,
    Disconnecting, Closed,
}

pub enum SessionEvent {
    RelayConnected,
    HandshakeComplete,
    HostAccepted,
    CapabilitiesNegotiated,
    VideoStarted,
    VideoProbeTimeout,
    DecoderStalled,
    Disconnected { retryable: bool },
    UserDisconnected,
}

pub enum SessionAction {
    SendControl(ControlMessage),
    StartVideo,
    RebuildDecoder,
    RequestKeyframe,
    Reconnect,
    ReleaseAll,
    Close,
}
~~~

SessionMachine::apply returns SessionError::InvalidTransition for StartVideo before HostAccepted and returns ReleaseAll before every reconnect or close action.

- [ ] Step 5: Run focused and workspace tests

~~~bash
cargo test -p desklink-session --test state_machine
cargo test --workspace
~~~

Expected: coordinate, sequence, approval, recovery and release tests pass.

- [ ] Step 6: Commit the session core

~~~bash
git add crates/desklink-session
git commit -m "feat(session): add state machine and normalized input"
~~~

## Task 5: 实现设备身份、临时配对和 Noise 会话

**Files:**

- Create: crates/desklink-crypto/src/identity.rs
- Create: crates/desklink-crypto/src/pairing.rs
- Create: crates/desklink-crypto/src/noise.rs
- Create: crates/desklink-crypto/src/lib.rs
- Create: crates/desklink-crypto/tests/handshake.rs
- Modify: crates/desklink-crypto/Cargo.toml

**Interfaces:**

- Produces DeviceIdentity, IdentityStore, PairingOffer, PairingCode, NoiseInitiator, NoiseResponder and EncryptedMessage.
- IdentityStore is a platform-independent trait; Windows DPAPI and Apple Keychain adapters are injected through the trait.
- Noise pattern is fixed to Noise_XX_25519_ChaChaPoly_BLAKE2s; no custom encryption algorithm is introduced.

- [ ] Step 1: Write failing identity and pairing tests

~~~rust
#[test]
fn identity_signature_verifies_only_for_original_payload() {
    let identity = DeviceIdentity::generate(&mut ChaCha20Rng::from_seed([7; 32]));
    let signature = identity.sign(b"desklink-handshake");
    assert!(identity.verify(b"desklink-handshake", &signature));
    assert!(!identity.verify(b"changed", &signature));
}

#[test]
fn pairing_code_expires_and_cannot_be_reused() {
    let offer = PairingOffer::new(SessionId::from_bytes([1; 16]), 1_000, 600);
    assert!(offer.validate_code(&offer.code().to_string(), 1_599).is_ok());
    assert!(matches!(
        offer.validate_code(&offer.code().to_string(), 1_600),
        Err(PairingError::Expired)
    ));
    offer.consume(1_599).unwrap();
    assert!(matches!(offer.consume(1_599), Err(PairingError::AlreadyConsumed)));
}

#[test]
fn noise_initiator_and_responder_produce_same_ciphertext_key() {
    let (mut initiator, message_1) = NoiseInitiator::start().unwrap();
    let (mut responder, message_2) = NoiseResponder::accept(&message_1).unwrap();
    let message_3 = initiator.receive(&message_2).unwrap();
    responder.receive(&message_3).unwrap();
    assert_eq!(
        initiator.finish().unwrap().session_key(),
        responder.finish().unwrap().session_key()
    );
}
~~~

- [ ] Step 2: Run focused tests and verify failure

~~~bash
cargo test -p desklink-crypto --test handshake
~~~

Expected: FAIL because identity, pairing and Noise interfaces are not defined.

- [ ] Step 3: Implement Ed25519 identity and injectable storage

~~~rust
pub trait IdentityStore {
    type Error;
    fn load(&self) -> Result<Option<DeviceIdentity>, Self::Error>;
    fn save(&self, identity: &DeviceIdentity) -> Result<(), Self::Error>;
}

pub struct DeviceIdentity {
    pub device_id: [u8; 16],
    signing_key: SigningKey,
}

impl DeviceIdentity {
    pub fn generate(rng: &mut impl CryptoRngCore) -> Self;
    pub fn verify_key(&self) -> VerifyingKey;
    pub fn sign(&self, payload: &[u8]) -> Signature;
    pub fn verify(&self, payload: &[u8], signature: &Signature) -> bool;
}
~~~

Private key bytes must be zeroized on drop. Production adapters must not serialize private keys into logs or protocol messages.

- [ ] Step 4: Implement expiring pairing offers

~~~rust
pub struct PairingOffer {
    session_id: SessionId,
    code: PairingCode,
    expires_at_unix_s: u64,
    consumed: bool,
}

impl PairingOffer {
    pub fn new(session_id: SessionId, now_unix_s: u64, ttl_s: u64) -> Self;
    pub fn code(&self) -> PairingCode;
    pub fn validate_code(&self, code: &str, now_unix_s: u64) -> Result<(), PairingError>;
    pub fn consume(&mut self, now_unix_s: u64) -> Result<(), PairingError>;
}
~~~

The code alphabet excludes visually ambiguous characters, has a fixed length of 8, and is generated from cryptographically secure randomness in production.

- [ ] Step 5: Implement Noise XX transport protection

NoiseInitiator and NoiseResponder wrap Snow handshake state, sign the transcript with the Ed25519 device identity, and expose:

~~~rust
pub fn write_message(&mut self, payload: &[u8]) -> Result<Vec<u8>, CryptoError>;
pub fn receive(&mut self, message: &[u8]) -> Result<Vec<u8>, CryptoError>;
pub fn finish(self) -> Result<TransportCipher, CryptoError>;

pub struct TransportCipher {
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, CryptoError>;
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, CryptoError>;
}
~~~

Reject messages above 64KiB before encryption or decryption and return stable errors for invalid signatures, invalid state, malformed handshake and authentication failure.

- [ ] Step 6: Run focused and workspace tests

~~~bash
cargo test -p desklink-crypto --test handshake
cargo test --workspace
~~~

Expected: signature, expiration, one-time use, handshake and tamper tests pass.

- [ ] Step 7: Commit the crypto boundary

~~~bash
git add crates/desklink-crypto
git commit -m "feat(crypto): add device identity and Noise pairing"
~~~

## Task 6: 实现 QUIC 通道和只转发密文的中继

**Files:**

- Create: crates/desklink-transport/src/lib.rs
- Create: crates/desklink-transport/src/quic.rs
- Create: crates/desklink-transport/tests/localhost.rs
- Create: server/relay/src/lib.rs
- Create: server/relay/src/main.rs
- Create: server/relay/tests/session.rs
- Modify: crates/desklink-transport/Cargo.toml
- Modify: server/relay/Cargo.toml

**Interfaces:**

- Produces RelayJoin, TransportEvent, QuicClient, RelaySessionTable and RelayError.
- The relay parses only the join envelope needed for session matching; video, input and control payloads are opaque encrypted bytes after the join.
- QuicClient exposes separate reliable control/input/config streams and unreliable video/cursor datagrams.

- [ ] Step 1: Write failing localhost relay tests

~~~rust
#[tokio::test]
async fn relay_matches_host_and_controller_and_forwards_opaque_bytes() {
    let relay = spawn_test_relay().await;
    let host = connect(&relay).await;
    let controller = connect(&relay).await;
    let session = SessionId::from_bytes([8; 16]);
    host.join(RelayJoin::host_with_participant(session, [4; 32], [1; 16])).await.unwrap();
    controller
        .join(RelayJoin::controller_with_participant(session, [4; 32], [2; 16]))
        .await
        .unwrap();
    host.send_video_datagram(vec![0, 1, 2, 255]).await.unwrap();
    assert_eq!(
        controller.next_event().await.unwrap(),
        TransportEvent::VideoDatagram(vec![0, 1, 2, 255])
    );
}

#[tokio::test]
async fn second_controller_is_rejected() {
    let table = RelaySessionTable::new(RelayConfig::default());
    table.attach_host(session(1), connection(1)).unwrap();
    table.attach_controller(session(1), connection(2)).unwrap();
    assert_eq!(
        table.attach_controller(session(1), connection(3)),
        Err(RelayError::SessionOccupied)
    );
}
~~~

- [ ] Step 2: Run focused tests and verify failure

~~~bash
cargo test -p desklink-transport --test localhost
cargo test -p desklink-relay --test session
~~~

Expected: FAIL because no QUIC client, session table or relay event exists.

- [ ] Step 3: Implement the transport event boundary

~~~rust
pub enum TransportEvent {
    Control(Vec<u8>),
    Input(Vec<u8>),
    VideoConfig(Vec<u8>),
    VideoDatagram(Vec<u8>),
    CursorDatagram(Vec<u8>),
    Closed { reason: String },
}

pub struct QuicClient { /* endpoint, connection and channel tasks */ }

impl QuicClient {
    pub async fn connect(config: QuicClientConfig) -> Result<Self, TransportError>;
    pub async fn join(&self, join: RelayJoin) -> Result<(), TransportError>;
    pub async fn send_control(&self, bytes: Vec<u8>) -> Result<(), TransportError>;
    pub async fn send_input(&self, bytes: Vec<u8>) -> Result<(), TransportError>;
    pub async fn send_video_config(&self, bytes: Vec<u8>) -> Result<(), TransportError>;
    pub async fn send_video_datagram(&self, bytes: Vec<u8>) -> Result<(), TransportError>;
    pub async fn next_event(&self) -> Result<TransportEvent, TransportError>;
}
~~~

Cap reliable messages at 64KiB and datagrams at 1200 bytes. Configure QUIC keepalive for 5 seconds and declare the connection dead after 15 seconds without heartbeat or transport activity.

- [ ] Step 4: Implement relay matching and opaque forwarding

RelaySessionTable stores at most one host and one controller per SessionId, plus creation time, expiry and connection IDs. attach_host and attach_controller return SessionOccupied when a role is already filled. detach removes only the specified connection. sweep(now) returns expired sessions.

The relay forwards original byte buffers without deserializing them after RelayJoin succeeds. It must not log payload bytes.

- [ ] Step 5: Run focused and workspace tests

~~~bash
cargo test -p desklink-transport --test localhost
cargo test -p desklink-relay --test session
cargo test --workspace
~~~

Expected: matching, opaque datagram forwarding, rejection, expiry and disconnect tests pass.

- [ ] Step 6: Commit transport and relay

~~~bash
git add crates/desklink-transport server/relay
git commit -m "feat(transport): add QUIC channels and opaque relay"
~~~

## Task 7: 暴露可被 Swift 调用的会话 FFI

**Files:**

- Create: crates/desklink-ffi/include/desklink.h
- Create: crates/desklink-ffi/src/lib.rs
- Create: crates/desklink-ffi/tests/abi.rs
- Modify: crates/desklink-ffi/Cargo.toml
- Modify: crates/desklink-session/src/lib.rs

**Interfaces:**

- Produces the C ABI used by RustBridge.swift; Swift never opens a Socket or accesses Rust internals.
- Callback payloads are valid only during the callback; Swift copies all byte buffers before returning.
- Every handle is destroyed exactly once by desklink_destroy.

- [ ] Step 1: Write failing FFI smoke test

~~~rust
#[test]
fn ffi_handle_can_be_created_and_destroyed() {
    let config = DesklinkConfig {
        log_level: 1,
        relay_url: ptr("quic://127.0.0.1:4433"),
    };
    let mut handle = null_mut();
    assert_eq!(
        unsafe { desklink_create(&config, None, null_mut(), &mut handle) },
        DesklinkResult::Ok
    );
    assert!(!handle.is_null());
    unsafe { desklink_destroy(handle) };
}
~~~

- [ ] Step 2: Run the FFI test and verify failure

~~~bash
cargo test -p desklink-ffi --test abi
~~~

Expected: FAIL because the C ABI types and functions are not defined.

- [ ] Step 3: Define the C header

desklink.h exposes these entry points:

~~~c
typedef struct DesklinkHandle DesklinkHandle;
typedef struct { const char *relay_url; uint32_t log_level; } DesklinkConfig;
typedef void (*DesklinkEventCallback)(void *context, const DesklinkEvent *event);

DesklinkResult desklink_create(const DesklinkConfig *, DesklinkEventCallback, void *, DesklinkHandle **);
DesklinkResult desklink_start_pairing(DesklinkHandle *, DesklinkPairingInfo *);
DesklinkResult desklink_connect_with_code(DesklinkHandle *, const char *code);
DesklinkResult desklink_accept(DesklinkHandle *);
DesklinkResult desklink_reject(DesklinkHandle *);
DesklinkResult desklink_send_input(DesklinkHandle *, const DesklinkInput *);
DesklinkResult desklink_request_keyframe(DesklinkHandle *);
DesklinkResult desklink_release_all(DesklinkHandle *);
void desklink_destroy(DesklinkHandle *);
~~~

Define event kinds for state changes, error, video config, H.264 access unit, cursor and metrics. The event struct carries data, data_len, stream_id, frame_id, config_version, width and height where relevant.

- [ ] Step 4: Implement FFI ownership and callback dispatch

Use an opaque Rust Box<DesklinkRuntime> behind DesklinkHandle. Validate null pointers and UTF-8 inputs, return InvalidArgument for invalid pointers, and keep a ReleaseAll action in both desklink_reject and desklink_destroy.

- [x] Rust `ControllerRuntime` 已完成真实 QUIC/Noise 握手、加密协商、VideoConfig/光标解密、视频重组、关键帧恢复和加密输入发送，并由真实 relay 双端测试覆盖；
- [x] `ControllerRuntime` 已进入 C ABI 可取消后台 worker；安全连接配置、真实事件回调、加密输入/关键帧命令、连接中取消、销毁等待和失败后重连均已接线；
- [x] macOS 已加入 Apple Keychain 控制端身份、Ed25519 验证公钥派生、开发期安全连接配置和 Swift Bridge/HomeView 入口；仍需在 macOS arm64 完成原生编译与跨机验收。

- [ ] Step 5: Run ABI and workspace tests

~~~bash
cargo test -p desklink-ffi --test abi
cargo test --workspace
~~~

Expected: handle lifecycle, null validation, callback event, keyframe request and release-all tests pass.

- [ ] Step 6: Commit the FFI boundary

~~~bash
git add crates/desklink-ffi crates/desklink-session/src/lib.rs
git commit -m "feat(ffi): expose typed session bridge to Apple clients"
~~~

## Task 8: 实现 macOS Apple Silicon 控制端

**Files:**

- Create: apps/macos/Package.swift
- Create: apps/macos/Info.plist
- Create: apps/macos/Sources/DeskLinkApp/DeskLinkApp.swift
- Create: apps/macos/Sources/DeskLinkApp/Bridge/RustBridge.swift
- Create: apps/macos/Sources/DeskLinkApp/Bridge/DeskLinkEvents.swift
- Create: apps/macos/Sources/DeskLinkApp/Views/HomeView.swift
- Create: apps/macos/Sources/DeskLinkApp/Views/ConnectView.swift
- Create: apps/macos/Sources/DeskLinkApp/Views/SessionView.swift
- Create: apps/macos/Sources/DeskLinkApp/Views/DiagnosticsView.swift
- Create: apps/macos/Sources/DeskLinkApp/Video/H264Decoder.swift
- Create: apps/macos/Sources/DeskLinkApp/Video/MetalVideoView.swift
- Create: apps/macos/Sources/DeskLinkApp/Input/InputMapper.swift
- Create: apps/macos/Tests/DeskLinkAppTests/InputMapperTests.swift
- Create: scripts/build-macos-arm64.sh

**Interfaces:**

- Consumes desklink.h from Task 7 and receives copied H.264 access units through RustBridge.
- Produces a macOS Apple Silicon .app with home, connect, session and diagnostics screens.
- H264Decoder publishes CVPixelBuffer; MetalVideoView consumes only the latest pixel buffer.

- [ ] Step 1: Write failing Swift input-mapping tests

~~~swift
final class InputMapperTests: XCTestCase {
    func testNormalisedPointUsesOnlyVisibleVideoRect() {
        let mapper = InputMapper(videoRect: CGRect(x: 100, y: 50, width: 800, height: 450))
        XCTAssertEqual(
            mapper.normalizedPoint(for: CGPoint(x: 500, y: 275)),
            CGPoint(x: 0.5, y: 0.5)
        )
        XCTAssertNil(mapper.normalizedPoint(for: CGPoint(x: 50, y: 275)))
    }

    func testCommandMapsToRemoteControlWhenAutomaticMappingIsEnabled() {
        let mapper = InputMapper(videoRect: .zero, modifierMode: .automatic)
        XCTAssertEqual(mapper.remoteModifier(for: .command), .control)
    }
}
~~~

- [ ] Step 2: Run Swift tests and verify failure

~~~bash
cd apps/macos
swift test --arch arm64
~~~

Expected: FAIL because the Swift package and InputMapper do not exist.

- [ ] Step 3: Create the Swift package and link Apple frameworks

Package.swift declares an arm64 macOS executable target with linker settings for VideoToolbox, Metal, MetalKit, CoreVideo, CoreGraphics, Carbon and Security. The build script supplies the C header and Rust static library path.

~~~bash
cargo build --release -p desklink-ffi --target aarch64-apple-darwin
swift build -c release --arch arm64 \
  -Xlinker -L../../target/aarch64-apple-darwin/release \
  -Xlinker -ldesklink_ffi
~~~

- [ ] Step 4: Implement Swift bridge and state views

RustBridge wraps the C functions from Task 7 and publishes ConnectionState, ConnectionError, PairingInfo and Metrics. HomeView shows device name, permission state, temporary code and connect entry. ConnectView validates code input and presents errors. SessionView renders the video surface and exposes disconnect, quality, aspect-ratio and diagnostics actions.

No view opens a network connection directly. All state changes go through RustBridge.

- [ ] Step 5: Implement VideoToolbox decoder with latest-frame presentation

H264Decoder creates a VTDecompressionSession after receiving SPS/PPS and dimensions. It drops access units with an older frame ID, requests a keyframe after three consecutive decode failures, and replaces the decoder when config_version changes. The output callback publishes only the newest CVPixelBuffer.

MetalVideoView uses CVMetalTextureCache, handles BGRA/NV12, preserves aspect ratio, and ignores points outside the visible video rectangle for input mapping.

- [x] VideoConfig、Annex B SPS/PPS 解析、Annex B → AVCC、异步最新帧发布、三次失败关键帧恢复与 Metal 等比黑边已接通；
- [x] Swift Bridge 已由真实 Rust QUIC/Noise C ABI worker 驱动，解密后的 VideoConfig、H.264 access unit 和连接状态会进入现有解码链路；
- [ ] 需在 macOS arm64 重新运行 Swift 测试、FFI 链接、打包检查和 Windows host 跨机验收。

- [ ] Step 6: Implement desktop pointer and keyboard input

InputMapper converts mouse locations inside the video rectangle to DesklinkInput values with x and y in 0...1. It maps Command to Control and Option to Alt in automatic mode, while raw mode preserves local modifier identity. It sends ReleaseAll from SessionView on disconnect and when the app leaves the foreground.

- [x] SessionView 已按实际等比画面区域归一化指针坐标，并把移动、左键按下/抬起及离开会话时的 `ReleaseAll` 送入安全 C ABI；
- [ ] 右键、中键、滚轮和原生键盘事件仍需在 macOS arm64 上完成接线与交互验收。

- [ ] Step 7: Run Swift and macOS packaging checks

~~~bash
cd apps/macos
swift test --arch arm64
cd ../..
./scripts/build-macos-arm64.sh --check
~~~

Expected: InputMapperTests pass, Rust FFI links for arm64, and the script verifies the .app executable is an arm64 Mach-O binary with required Info.plist keys.

- [ ] Step 8: Commit the macOS controller

~~~bash
git add apps/macos scripts/build-macos-arm64.sh
git commit -m "feat(macos): add Apple Silicon controller shell"
~~~

## Task 9: 实现 Windows 被控端采集、编码和输入注入

**Files:**

- Create: apps/windows/Cargo.toml
- Create: apps/windows/src/main.rs
- Create: apps/windows/src/window.rs
- Create: apps/windows/src/capture.rs
- Create: apps/windows/src/encoder.rs
- Create: apps/windows/src/input.rs
- Create: apps/windows/tests/capture_smoke.rs
- Create: scripts/build-windows.ps1

**Interfaces:**

- Consumes VideoFrameHeader, InputEvent, LatestFrameQueue, SessionMachine and QuicClient from earlier tasks.
- Produces DesktopCapturer, H264Encoder, InputInjector and a host runtime that sends encoded frames and applies remote input.
- Windows-specific modules compile only under cfg(windows); common protocol and session tests remain runnable on macOS.

- [x] Step 1: Write failing Windows-only capture test

~~~rust
#[cfg(windows)]
#[test]
fn primary_display_capture_reports_non_zero_dimensions() {
    let mut capture = DxgiDesktopCapturer::new_primary().expect("capture init");
    let frame = capture.next_frame(Duration::from_millis(500)).expect("frame");
    assert!(frame.width > 0);
    assert!(frame.height > 0);
}
~~~

Run on Windows:

~~~powershell
cargo test --manifest-path apps/windows/Cargo.toml --test capture_smoke -- --nocapture
~~~

Expected before implementation: FAIL because DxgiDesktopCapturer is not defined.

- [x] Step 2: Implement D3D11/DXGI Desktop Duplication

~~~rust
pub trait DesktopCapturer {
    fn next_frame(&mut self, timeout: Duration) -> Result<CapturedFrame, CaptureError>;
    fn dimensions(&self) -> (u32, u32);
}

pub struct DxgiDesktopCapturer {
    device: ID3D11Device,
    duplication: IDXGIOutputDuplication,
    dimensions: (u32, u32),
}
~~~

Create a D3D11 device, select the primary IDXGIOutput, call DuplicateOutput, acquire frames with AcquireNextFrame, copy the GPU texture reference into the encoder queue, and always call ReleaseFrame including timeout and error paths. Handle DXGI_ERROR_ACCESS_LOST by recreating duplication.

- [x] Step 3: Implement Media Foundation H.264 encoding

~~~rust
pub struct H264Encoder {
    width: u32,
    height: u32,
    frame_id: u64,
    config_version: u32,
}

impl H264Encoder {
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, EncoderError>;
    pub fn encode(&mut self, frame: CapturedFrame, force_keyframe: bool) -> Result<EncodedFrame, EncoderError>;
    pub fn rebuild(&mut self, width: u32, height: u32) -> Result<(), EncoderError>;
}
~~~

Configure Media Foundation H.264 for real-time 30FPS, no B-frames, one-second keyframe interval, 4Mbps starting bitrate and 1920×1080 maximum output. Keep the encode queue at two frames and discard the oldest unencoded frame when full.

- [x] Step 4: Implement SendInput and safe release

InputInjector maps normalized coordinates to the virtual desktop rectangle, maps logical/physical keys to Windows virtual keys and scan codes, emits Unicode text events, and tracks pressed modifiers/buttons. release_all emits key-up and button-up events for every tracked input before clearing the set. Elevated/UAC targets return InputInjectionBlocked without crashing the host.

- [x] 归一化 `1.0` 已映射到虚拟桌面的最后一个有效像素，不再越界一像素；
- [x] 共享协议、C ABI 和 Windows `SendInput` 已支持有界水平/垂直滚轮以及 Shift/Control/Alt/Meta 组合键；
- [x] Windows 批量注入支持 Unicode UTF-16 代理对、扩展方向键和部分失败后的组合键清理，mock 后端验证滚轮不进入 pressed-state、`ReleaseAll` 保留原修饰键并可重试。

- [x] Step 5: Implement the Windows host runtime

main.rs starts the identity/session runtime, creates primary-display capture and encoder, handles connection approval in window.rs, sends VideoConfig followed by an IDR, handles RequestKeyframe, sends cursor updates independently, and dispatches InputEvent through InputInjector without waiting on the video queue.

- [x] Relay session、DXGI/MFT 专用线程、VideoConfig/IDR、关键帧请求、独立光标与输入调度已接通；
- [x] DPAPI 持久设备身份、Noise 双向认证，以及控制/输入/视频配置/视频/光标分通道 AEAD 已接通；可信控制端公钥仍由环境变量注入，等待配对与可信设备 UI。

- [x] Step 6: Run Windows checks

~~~powershell
rustup target add x86_64-pc-windows-msvc
cargo fmt --all -- --check
cargo check --manifest-path apps/windows/Cargo.toml --target x86_64-pc-windows-msvc
cargo test --manifest-path apps/windows/Cargo.toml --test capture_smoke -- --nocapture
.\scripts\build-windows.ps1 -Configuration Release -CheckOnly
~~~

Expected: common crates compile, Windows host links against D3D11/DXGI/Media Foundation, capture smoke reports primary display dimensions, and the script verifies executable architecture.

- [ ] Step 7: Commit the Windows host

~~~bash
git add apps/windows scripts/build-windows.ps1
git commit -m "feat(windows): add desktop capture host"
~~~

## Task 10: 接通端到端回环、诊断指标和验收脚本

**Files:**

- Create: tests/end-to-end/Cargo.toml
- Create: tests/end-to-end/src/main.rs
- Modify: scripts/verify.sh
- Modify: README.md
- Modify: docs/superpowers/specs/2026-07-15-desklink-design.md

**Interfaces:**

- Consumes the real protocol, video assembler, session machine, crypto and transport crates; it does not duplicate frame ordering logic.
- Produces a deterministic local test that sends synthetic H.264 access units, drops selected datagrams, requests a keyframe and proves that the newest frame is displayed.
- Produces verification commands for macOS arm64 and Windows MSVC environments.

- [ ] Step 1: Write failing end-to-end assertions

~~~rust
#[tokio::test]
async fn dropped_old_frame_recovers_with_new_keyframe() {
    let mut harness = Harness::new().await;
    harness.send_frame(1, false).await;
    harness.drop_next_frame(2).await;
    harness.send_frame(3, true).await;
    assert_eq!(harness.last_presented_frame().await, Some(3));
    assert_eq!(harness.keyframe_requests().await, 1);
}

#[tokio::test]
async fn input_is_delivered_while_video_queue_is_full() {
    let mut harness = Harness::new().await;
    harness.fill_video_queue_with_frames(10).await;
    harness.send_input(InputEvent::PointerMove { x: 0.5, y: 0.5 }).await;
    assert_eq!(harness.received_input_count().await, 1);
}
~~~

- [ ] Step 2: Run the end-to-end tests and verify failure

~~~bash
cargo test --manifest-path tests/end-to-end/Cargo.toml
~~~

Expected: FAIL because the harness and recovery counters are not defined.

- [ ] Step 3: Implement the deterministic harness

The harness uses QuicClient with an in-process relay endpoint, FrameAssembler::push, SessionMachine::apply, a fake decoder that fails once for a selected frame, and a fake renderer that records only accepted stream/frame pairs. It uses a fixed seed for synthetic H.264 payload bytes and a fake clock for the 500ms and 800ms recovery thresholds.

- [ ] Step 4: Add diagnostics assertions

~~~rust
pub struct SessionMetrics {
    pub rtt_ms: u32,
    pub capture_fps: f32,
    pub encode_fps: f32,
    pub send_mbps: f32,
    pub receive_mbps: f32,
    pub complete_fps: f32,
    pub decode_fps: f32,
    pub present_fps: f32,
    pub dropped_frames: u64,
    pub last_frame_id: u64,
    pub video_delay_ms: u32,
    pub input_sequence: u64,
    pub stream_id: u64,
    pub config_version: u32,
}
~~~

Metrics contain no screen, keyboard or clipboard content and are safe to show in the macOS diagnostics view.

- [ ] Step 5: Run the complete verification suite

~~~bash
./scripts/verify.sh
cargo test --manifest-path tests/end-to-end/Cargo.toml
cd apps/macos && swift test --arch arm64 && cd ../..
~~~

Expected: Rust formatting, Clippy, all unit/integration tests, deterministic recovery tests and Swift input tests pass. The Windows capture command remains a separately documented Windows-machine check.

- [ ] Step 6: Update implementation documentation

Add verified commands, target names, current limitations and actual test results to README.md and the design specification. Do not claim Windows capture or full remote control passed until the Windows-machine command has produced a passing result.

- [ ] Step 7: Commit the vertical-slice verification

~~~bash
git add tests/end-to-end scripts/verify.sh README.md docs/superpowers/specs/2026-07-15-desklink-design.md
git commit -m "test: verify DeskLink desktop vertical slice"
~~~

---

## Spec Coverage Review

| Design requirement | Plan coverage |
|---|---|
| Rust shared protocol and session core | Tasks 1–5 |
| Latest-frame-first video behavior | Task 3 |
| Frame IDs, stream IDs, config versions and keyframe recovery | Tasks 2, 3 and 10 |
| Independent reliable input and ReleaseAll | Tasks 4 and 9 |
| Ed25519 identity, temporary code, Noise and no secret logging | Task 5 |
| QUIC channels and opaque relay | Task 6 |
| Stable C ABI for Swift/iOS | Task 7 |
| macOS Apple Silicon controller | Task 8 |
| Windows Desktop Duplication, Media Foundation and SendInput | Task 9 |
| Error states and metrics | Tasks 4, 7 and 10 |
| Manual approval and single controller | Tasks 4, 5 and 6 |
| MVP UI and diagnostics | Task 8 |
| Regression, packet loss and freeze recovery | Tasks 3 and 10 |
| iOS controller | Intentionally excluded from this first vertical-slice plan; the C ABI is the integration boundary for its separate plan. |
| macOS host | Intentionally excluded from this first vertical-slice plan; it follows after Windows-to-macOS control is stable. |

## Self-Review Checklist

- The plan contains no unresolved placeholder marker.
- All later interfaces refer to names introduced in an earlier task.
- Every task has a failing test, a command that demonstrates failure, an implementation boundary, a passing command and a commit.
- The macOS target is consistently Apple Silicon only.
- The first plan is limited to one independently testable vertical slice; macOS host, Windows controller and iOS controller are separate follow-up plans.
