# Windows 0.1.58 Video Continuity Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Detect broken H.264 reference chains at the shared controller boundary, stop forwarding undecodable delta frames, and recover with bounded encrypted keyframe requests.

**Architecture:** A pure `VideoContinuity` state machine decides whether a completed encoded frame may be presented. `ControllerRuntime` feeds it confirmed fragment loss and frame IDs, sends the existing encrypted `RequestKeyframe` message when requested, and otherwise leaves the transport, relay, host, and Windows UI unchanged.

**Tech Stack:** Rust 2024, Tokio, QUIC datagrams, Noise encrypted control messages, existing `desklink-video` frame assembler, Cargo workspace tests, Bun Windows UI verification, Python release scripts.

## Global Constraints

- Windows 10/11 x64 is the release target; do not modify macOS application code.
- Keep `PROTOCOL_VERSION` at `9`; no relay deployment or wire-format change is allowed.
- Keyframe retry cooldown is exactly `Duration::from_secs(1)`.
- Recovery may drop video delta frames only; it must not stop the session or block input, audio, clipboard, or file transfer.
- Product version becomes `0.1.58` in all five Windows version sources and matching workspace lock entries.
- Use Chinese user-facing documentation; no new technical status is added to the main UI.
- Work in the existing dirty `main` workspace because 0.1.56 and 0.1.57 are direct uncommitted dependencies; do not commit or push unless the user asks.

---

### Task 1: Pure Video Continuity State Machine

**Files:**
- Create: `crates/desklink-ffi/src/video_continuity.rs`
- Modify: `crates/desklink-ffi/src/lib.rs`

**Interfaces:**
- Produces: `pub(crate) const KEYFRAME_RETRY_INTERVAL: Duration`
- Produces: `pub(crate) enum VideoContinuityAction { Present, Drop, DropAndRequestKeyframe }`
- Produces: `pub(crate) struct VideoContinuity`
- Produces: `reset_for_config(&mut self)`, `note_transport_loss(&mut self)`, and `observe_frame(&mut self, frame_id: u64, is_keyframe: bool, now: Instant) -> VideoContinuityAction`

- [ ] **Step 1: Write failing state-machine tests**

Add tests before implementation for:

```rust
assert_eq!(continuity.observe_frame(10, true, now), VideoContinuityAction::Present);
assert_eq!(continuity.observe_frame(11, false, now), VideoContinuityAction::Present);
assert_eq!(continuity.observe_frame(13, false, now), VideoContinuityAction::DropAndRequestKeyframe);
assert_eq!(continuity.observe_frame(14, false, now + Duration::from_millis(999)), VideoContinuityAction::Drop);
assert_eq!(continuity.observe_frame(15, false, now + Duration::from_secs(1)), VideoContinuityAction::DropAndRequestKeyframe);
assert_eq!(continuity.observe_frame(16, true, now + Duration::from_secs(1)), VideoContinuityAction::Present);
```

Also cover `note_transport_loss`, `reset_for_config`, and the `u64::MAX -> 0` wrapping transition.

- [ ] **Step 2: Run the focused test and verify RED**

Run:

```powershell
python -X utf8 scripts/run-windows-cargo.py test -p desklink-ffi video_continuity --lib -- --nocapture
```

Expected: compilation or assertion failure because the production state machine is not implemented.

- [ ] **Step 3: Implement the minimal state machine**

Use these fields and decision order:

```rust
pub(crate) struct VideoContinuity {
    last_presented_frame_id: Option<u64>,
    awaiting_keyframe: bool,
    last_keyframe_request_at: Option<Instant>,
}
```

Keyframes always establish a new continuity point. For delta frames, a non-wrapping-contiguous ID or prior transport loss sets `awaiting_keyframe`; while awaiting, return `DropAndRequestKeyframe` only when the one-second cooldown has elapsed.

- [ ] **Step 4: Run focused tests and verify GREEN**

Run the command from Step 2. Expected: all `video_continuity` tests pass with zero failures.

- [ ] **Step 5: Review checkpoint**

Run `cargo fmt --all -- --check` and inspect only the Task 1 diff. Do not commit in this dirty chained workspace.

### Task 2: Controller Runtime Integration

**Files:**
- Modify: `crates/desklink-ffi/src/controller.rs`
- Test: `crates/desklink-ffi/tests/controller_runtime.rs`

**Interfaces:**
- Consumes: `VideoContinuity` and `VideoContinuityAction` from Task 1.
- Preserves: `ControllerEvent::H264AccessUnit(EncodedFrame)` and existing `ControlMessage::RequestKeyframe { stream_id }`.

- [ ] **Step 1: Write a failing encrypted integration test**

Create a localhost relay test whose fake host sends:

```text
VideoConfig(stream 9) -> keyframe 10 -> delta frame 12
```

Assert that the controller emits frame 10, does not emit frame 12, and the fake host receives `RequestKeyframe { stream_id: 9 }`. Then send keyframe 13 and assert it is emitted, followed by delta frame 14 which must also be emitted.

- [ ] **Step 2: Run the integration test and verify RED**

Run:

```powershell
python -X utf8 scripts/run-windows-cargo.py test -p desklink-ffi --test controller_runtime controller_runtime_requests_a_keyframe_after_a_reference_gap -- --nocapture
```

Expected: frame 12 is incorrectly emitted or the automatic keyframe request times out.

- [ ] **Step 3: Wire continuity decisions into `ControllerRuntime`**

Replace the separate `awaiting_keyframe` and `keyframe_request_outstanding` booleans with one `VideoContinuity`. On video configuration changes call `reset_for_config`. When `FrameAssembler::take_dropped_chunks()` is non-zero call `note_transport_loss`. For each complete frame:

```rust
match self.video_continuity.observe_frame(frame.frame_id, is_keyframe, Instant::now()) {
    VideoContinuityAction::Present => { /* existing accept and emit path */ }
    VideoContinuityAction::Drop => self.drop_video_packet(),
    VideoContinuityAction::DropAndRequestKeyframe => {
        self.drop_video_packet();
        self.request_keyframe_for(config.stream_id).await?;
    }
}
```

When video arrived before its configuration, retain `keyframe_needed_after_config`; after sending that deferred request, mark the continuity request time so repeated delta frames obey the same cooldown.

- [ ] **Step 4: Run integration and crate tests and verify GREEN**

Run:

```powershell
python -X utf8 scripts/run-windows-cargo.py test -p desklink-ffi --test controller_runtime -- --nocapture
python -X utf8 scripts/run-windows-cargo.py test -p desklink-ffi --all-targets
```

Expected: the new recovery test and every existing FFI target pass.

- [ ] **Step 5: Review checkpoint**

Confirm the runtime never sends a keyframe request more than once per second during continuous delta-frame arrival, and that keyframe receipt immediately clears recovery state. Do not commit.

### Task 3: Product Version and Documentation

**Files:**
- Modify: `apps/windows/Cargo.toml`
- Modify: `apps/windows-ui/src-tauri/Cargo.toml`
- Modify: `apps/windows-ui/package.json`
- Modify: `apps/windows-ui/src-tauri/tauri.conf.json`
- Modify: `tools/windows-installer/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `README.md`
- Modify: `docs/windows-architecture-review.md`
- Create: `docs/windows-0.1.58-video-continuity-recovery.md`

**Interfaces:**
- Produces: five source version values equal to `0.1.58` and matching workspace package lock versions.
- Preserves: protocol 9 and current managed relay endpoints.

- [ ] **Step 1: Bump Windows product versions**

Change only DeskLink Windows application/package versions from `0.1.57` to `0.1.58`. Do not alter dependency versions or the protocol constant.

- [ ] **Step 2: Document the recovery behavior**

State explicitly that fragment loss or a frame-number gap closes the current delta reference chain, keyframe requests retry at most once per second, received keyframes resume delivery, both computers should use 0.1.58, and the relay requires no deployment.

- [ ] **Step 3: Verify version invariants**

Use a UTF-8 Python check to assert all five sources contain `0.1.58`, none contains `0.1.57`, `PROTOCOL_VERSION: u16 = 9` remains present, and the new release note contains the one-second bound.

- [ ] **Step 4: Review checkpoint**

Run `git diff --check`. Expected: exit code 0. Do not commit.

### Task 4: Full Verification and Windows Installer

**Files:**
- Generate ignored artifact: `dist/windows/DeskLinkSetup-0.1.58-x64.exe`
- Generate ignored manifests under `dist/windows/`

**Interfaces:**
- Consumes: all implementation and version changes from Tasks 1-3.
- Produces: verified unsigned Windows x64 test installer and SHA-256 digest.

- [ ] **Step 1: Run frontend and script tests**

```powershell
cd apps/windows-ui
bun install --frozen-lockfile
bun test
bun run build
cd ../..
python -X utf8 -m unittest discover -s tests -p "test_*.py"
```

Expected: every command exits 0.

- [ ] **Step 2: Run full Rust quality gates**

```powershell
cargo fmt --all -- --check
python -X utf8 scripts/run-windows-cargo.py test --workspace --all-targets --all-features --jobs 1
python -X utf8 scripts/run-windows-cargo.py clippy --workspace --all-targets --all-features -- -D warnings
```

Expected: every command exits 0 with no test or lint failure.

- [ ] **Step 3: Verify the managed relay without deploying it**

```powershell
python -X utf8 scripts/verify-managed-relay.py
```

Expected: TLS/QUIC and bidirectional protocol 9 probe passes.

- [ ] **Step 4: Build and verify the installer**

```powershell
python -X utf8 scripts/build-windows-installer.py
```

Expected: `DeskLinkSetup-0.1.58-x64.exe` exists; release and installer manifests report `passed: true`, version `0.1.58`, x64, and unsigned test status.

- [ ] **Step 5: Final requirements audit**

Run a UTF-8 Python script that checks the artifact hash, protocol 9, continuity wiring, one-second retry constant, version sources, manifests, `git diff --check`, branch name, and dirty-file scope. Report that physical two-PC loss testing remains the user's manual acceptance step and that no commit or push occurred.
