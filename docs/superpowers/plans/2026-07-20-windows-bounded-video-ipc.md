# Windows 0.1.59 Bounded Video IPC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace unacknowledged large-frame Tauri Channel pushes with a stream-bound single-slot pull mailbox so the Windows WebView can never accumulate an unbounded queue of stale H.264 frames.

**Architecture:** Move the existing reference-chain gate into `desklink-video`, then reuse it at both the QUIC reassembly boundary and a new Windows Tauri video mailbox. The WebView keeps exactly one `invoke` pull in flight; the Rust mailbox keeps one pending frame and requests a keyframe plus records playback pressure when a delta frame overflows that slot.

**Tech Stack:** Rust 2024, Tokio `Notify`, Tauri 2.11.5 raw `Response`, TypeScript, Bun tests, WebCodecs, existing Noise/QUIC protocol 9.

**Implementation status (2026-07-20):** Completed in the existing uncommitted `main` workspace. All focused and full gates, the live relay probe, and the installer audit passed.

## Global Constraints

- Windows 10/11 x64 is the product target; do not modify `apps/macos`.
- Keep `PROTOCOL_VERSION` equal to `9`; no relay or wire-format change.
- The Rust video mailbox pending capacity is exactly `1`.
- The frontend has at most one video pull in flight for one exact `(streamId, configVersion)` key.
- Keyframe recovery cooldown remains exactly `Duration::from_secs(1)`.
- Audio and small controller signals remain on Tauri Channel; only large H.264 delivery changes.
- Video overflow must not stop or block input, audio, clipboard, or file transfer.
- Product version becomes `0.1.59` in the five Windows version sources and matching lock entries.
- Continue in the existing dirty `main` workspace because 0.1.56-0.1.58 are uncommitted dependencies; do not commit or push.

---

### Task 1: Share the Reference-Chain Gate

**Files:**
- Create: `crates/desklink-video/src/continuity.rs`
- Modify: `crates/desklink-video/src/lib.rs`
- Modify: `crates/desklink-ffi/src/controller.rs`
- Modify: `crates/desklink-ffi/src/lib.rs`
- Delete: `crates/desklink-ffi/src/video_continuity.rs`

**Interfaces:**
- Produces: public `VideoContinuity`, `VideoContinuityAction`, and `KEYFRAME_RETRY_INTERVAL` from `desklink-video`.
- Preserves: `reset_for_config`, `note_transport_loss`, `note_keyframe_request`, and `observe_frame` signatures and behavior.

- [ ] **Step 1: Move the existing tests with the implementation**

Place the 0.1.58 tests beside `desklink-video::VideoContinuity` and export the three public items from `crates/desklink-video/src/lib.rs`.

- [ ] **Step 2: Rewire FFI to the shared export**

Use:

```rust
use desklink_video::{
    AssembleResult, EncodedFrame, FrameAssembler, VideoContinuity, VideoContinuityAction,
};
```

Remove the private FFI module declaration and file.

- [ ] **Step 3: Verify the behavior-preserving refactor**

```powershell
python -X utf8 scripts/run-windows-cargo.py test -p desklink-video
python -X utf8 scripts/run-windows-cargo.py test -p desklink-ffi --all-targets
```

Expected: all moved unit tests and both existing encrypted controller runtime tests pass.

- [ ] **Step 4: Review checkpoint**

Run `cargo fmt --all -- --check`; inspect the Task 1 diff and do not commit.

### Task 2: Single-Slot Video Mailbox

**Files:**
- Create: `apps/windows-ui/src-tauri/src/video_mailbox.rs`
- Modify: `apps/windows-ui/src-tauri/src/lib.rs`
- Modify: `apps/windows-ui/src-tauri/Cargo.toml`

**Interfaces:**
- Consumes: `desklink_video::{VideoContinuity, VideoContinuityAction}`.
- Produces: `VideoMailboxKey { stream_id: u64, config_version: u32 }`.
- Produces: `VideoDeliveryFrame { key, frame_id, keyframe, payload }`.
- Produces: `VideoMailboxOffer::{Queued, Dropped, RequestKeyframe, Ignored}`.
- Produces: `ControllerVideoMailbox::{begin_config, offer, next, close}`.

- [ ] **Step 1: Write failing mailbox tests**

Tests must demonstrate:

```rust
mailbox.begin_config(VideoMailboxKey::new(9, 3));
assert_eq!(mailbox.offer(keyframe(10), now), VideoMailboxOffer::Queued);
assert_eq!(mailbox.offer(delta(11), now), VideoMailboxOffer::RequestKeyframe);
assert_eq!(mailbox.next(key).await.unwrap().frame_id, 10);
```

Also test that a newer keyframe replaces a full slot, retry remains cooled for 999 ms and reopens at 1 second, duplicate `begin_config` is a no-op, configuration changes and `close` wake old waiters, and wrong keys cannot consume frames.

- [ ] **Step 2: Run focused tests and verify RED**

```powershell
python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows-ui video_mailbox --lib -- --nocapture
```

Expected: compile failure because mailbox production types are absent.

- [ ] **Step 3: Implement the minimal mailbox**

Use one `std::sync::Mutex<VideoMailboxState>` and one `tokio::sync::Notify`. State contains the active key, `VecDeque<VideoDeliveryFrame>`, closed flag, and `VideoContinuity`. Never hold the mutex across `.await`.

For a full slot:

```rust
if frame.keyframe {
    state.frames.clear();
    state.frames.push_back(frame);
    VideoMailboxOffer::Queued
} else {
    state.continuity.note_transport_loss();
    state.continuity.note_keyframe_request(now);
    VideoMailboxOffer::RequestKeyframe
}
```

Preserve the already queued safe frame when a delta overflows.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run the command from Step 2. Expected: every mailbox test passes with no panic or leaked waiter.

- [ ] **Step 5: Review checkpoint**

Confirm the queue can never exceed one frame and every lifecycle transition notifies waiters. Do not commit.

### Task 3: Rust Controller Pull Boundary

**Files:**
- Modify: `apps/windows-ui/src-tauri/src/controller.rs`
- Modify: `apps/windows-ui/src-tauri/src/lib.rs`

**Interfaces:**
- Produces: camelCase `ControllerVideoPullInput { stream_id, config_version }`.
- Produces: `ControllerManager::next_video_frame(input) -> Result<Vec<u8>, String>`.
- Produces Tauri command: `next_controller_video_frame(input) -> Result<Response, String>`.
- Removes: `video: Channel<Response>` from all controller connect/start/output signatures.

- [ ] **Step 1: Write failing manager boundary tests**

Add tests that a connected manager can begin key `(9, 3)`, queue one payload, return it only for `(9, 3)`, reject `(9, 4)`, and merge one bounded freshness recovery when a delta overflows the mailbox.

- [ ] **Step 2: Run focused tests and verify RED**

```powershell
python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows-ui controller_video_mailbox --lib -- --nocapture
```

Expected: compile or assertion failure because the manager is not wired.

- [ ] **Step 3: Remove the large video Channel path**

Remove the video Channel argument from `connect_device`, `connect_saved_device`, `reconnect_controller`, `ControllerManager::start`, and `ControllerOutputChannels`. Keep signals and audio unchanged.

- [ ] **Step 4: Offer frames to the mailbox**

On `VideoConfig`, call `begin_config` before publishing the signal. On `H264AccessUnit`, preserve the current 17-byte prefix and call `offer`.

For `RequestKeyframe`:

```rust
let _ = manager.record_playback_pressure(ControllerPlaybackPressure {
    stream_id: frame.stream_id,
    peak_decode_queue_size: 5,
    freshness_recoveries: 1,
});
if let Err(error) = runtime.request_keyframe().await {
    break ConnectFailure::from_controller(error);
}
```

- [ ] **Step 5: Close the mailbox on every attempt boundary**

Close before a reconnect attempt, after an attempt fails, and in the spawned worker wrapper after `run_controller` returns. A new `VideoConfig` is the only operation that reopens a key.

- [ ] **Step 6: Register and test the pull command**

Return `Response::new(payload)` so JavaScript receives the same binary shape. Run:

```powershell
python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows-ui --lib
```

Expected: all Tauri controller tests pass.

### Task 4: Serial Frontend Pull Loop

**Files:**
- Create: `apps/windows-ui/src/video-pull-loop.ts`
- Create: `apps/windows-ui/src/video-pull-loop.test.ts`
- Modify: `apps/windows-ui/src/api.ts`
- Modify: `apps/windows-ui/src/controller.ts`
- Modify: `apps/windows-ui/src/types.ts`

**Interfaces:**
- Produces: `VideoPullKey { streamId: number, configVersion: number }`.
- Produces: `SerialVideoPull<T>::start(key, pull, deliver, onDeliveryError)` and `stop()`.
- Produces API: `nextControllerVideoFrame(input: ControllerVideoPullInput): Promise<VideoPayload>`.

- [ ] **Step 1: Write failing TypeScript tests**

Tests must prove that only one `pull` promise is in flight, resolving a frame starts exactly one next pull, `stop` ignores a late result, a new key invalidates the old loop, starting the same key twice is a no-op, and a delivery exception calls the error handler but continues pulling.

- [ ] **Step 2: Run focused tests and verify RED**

```powershell
cd apps/windows-ui
bun test src/video-pull-loop.test.ts
```

Expected: module-not-found or missing-export failure.

- [ ] **Step 3: Implement the serial coordinator**

Use a monotonically increasing generation. Check generation and exact key both before delivery and before requesting the next frame. Pull rejection ends only that generation; delivery rejection is reported and the same generation continues.

- [ ] **Step 4: Replace frontend Channel video callbacks**

Remove `video` from `ControllerChannels` and remove `onVideo`/`onVideoError` from `createControllerChannels`. In `setupRemoteDesktop`, start the pull coordinator after the decoder and canvas are ready. `resetVideoTelemetry` must call `stop()`.

- [ ] **Step 5: Verify focused and full frontend tests**

```powershell
bun test src/video-pull-loop.test.ts
bun test
bun run build
```

Expected: focused tests, the complete suite, TypeScript, and Vite production build all pass.

### Task 5: Version, Documentation, and Release Verification

**Files:**
- Modify: `apps/windows/Cargo.toml`
- Modify: `apps/windows-ui/src-tauri/Cargo.toml`
- Modify: `apps/windows-ui/package.json`
- Modify: `apps/windows-ui/src-tauri/tauri.conf.json`
- Modify: `tools/windows-installer/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `README.md`
- Modify: `docs/windows-architecture-review.md`
- Create: `docs/windows-0.1.59-bounded-video-ipc.md`
- Generate ignored artifact: `dist/windows/DeskLinkSetup-0.1.59-x64.exe`

**Interfaces:**
- Produces: version `0.1.59`, unchanged protocol 9, verified unsigned x64 installer.

- [ ] **Step 1: Bump only Windows product versions**

Change the five product sources and three matching workspace lock packages from `0.1.58` to `0.1.59`. Do not change dependency versions or `PROTOCOL_VERSION`.

- [ ] **Step 2: Document the bounded IPC behavior**

Explain the Tauri large-payload queue risk, one in-flight plus one pending bound, reference-safe overflow recovery, automatic quality feedback, protocol compatibility, and no relay deployment.

- [ ] **Step 3: Run all quality gates**

```powershell
cargo fmt --all -- --check
python -X utf8 scripts/run-windows-cargo.py test --workspace --all-targets --all-features --jobs 1
python -X utf8 scripts/run-windows-cargo.py clippy --workspace --all-targets --all-features -- -D warnings
python -X utf8 -m unittest discover -s scripts/tests -p test_*.py
cd apps/windows-ui
bun install --frozen-lockfile
bun test
bun run build
```

Expected: every command exits 0.

- [ ] **Step 4: Probe the unchanged relay**

```powershell
python -X utf8 scripts/verify-managed-relay.py
```

Expected: production TLS/QUIC directory and bidirectional protocol 9 control probe passes.

- [ ] **Step 5: Build and audit the installer**

```powershell
python -X utf8 scripts/build-windows-installer.py
```

Verify both manifests report `passed: true`, version `0.1.59`, x64, `custom_protocol: true`, and unsigned test status. Independently hash the installer and compare it with the manifest.

- [ ] **Step 6: Final scope audit**

Use a UTF-8 Python script to check the mailbox capacity, removal of `video: Channel`, serial frontend pull, protocol 9, versions, no macOS changes, `git diff --check`, branch, and dirty-file scope. Keep `main` uncommitted and unpushed.
