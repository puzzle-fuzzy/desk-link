# Windows 0.1.60 Video Delivery Self-Healing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the bounded Windows video pull recover from transient IPC rejection and expose enough redacted cumulative metrics to distinguish network, Rust mailbox, pull, decode, and paint failures.

**Architecture:** Keep the single in-flight frontend pull and single-slot Rust mailbox. Add capped retry delays to the frontend coordinator, cumulative per-attempt mailbox counters to Rust, additive local diagnostic fields, and backward-compatible server analysis that treats absent fields as legacy evidence rather than failure.

**Tech Stack:** TypeScript, Bun tests, Tauri 2.11.5, Rust 2024, Tokio, existing signed diagnostic JSON batches, Bun diagnostics service, protocol 9.

## Global Constraints

- Windows 10/11 x64 only; do not modify `apps/macos`.
- Keep `PROTOCOL_VERSION` equal to `9`; do not modify relay or Noise wire formats.
- Keep one frontend pull in flight and one Rust mailbox frame pending.
- Retry delays are exactly `100, 250, 500, 1000, 2000` ms and remain capped at `2000` ms.
- Do not add technical counters to the main UI or upload secrets/content.
- Product version becomes `0.1.60` only after behavior and diagnostics tests pass.
- Do not deploy the diagnostics service from a dirty tree and do not commit/push without explicit user instruction.

---

### Task 1: Retrying Serial Video Pull

**Files:**
- Modify: `apps/windows-ui/src/video-pull-loop.test.ts`
- Modify: `apps/windows-ui/src/video-pull-loop.ts`
- Modify: `apps/windows-ui/src/controller.ts`
- Modify: `apps/windows-ui/src/types.ts`
- Modify: `apps/windows-ui/src-tauri/src/controller.rs`

**Interfaces:**
- Produces: `videoPullRetryDelay(consecutiveFailures: number): number`.
- Extends: `SerialVideoPull<T>::start` with `onPullError(error, consecutiveFailures, retryDelayMs)`.
- Extends: `ControllerRenderMetrics.videoPullFailures`.

- [ ] **Step 1: Write failing retry tests**

Add tests proving delays `100/250/500/1000/2000/2000`, success resets failure count, only one request remains in flight, and stop/config switch during a wait prevents another request.

```ts
expect([1, 2, 3, 4, 5, 6].map(videoPullRetryDelay)).toEqual([
  100, 250, 500, 1_000, 2_000, 2_000,
]);

const waits: Array<{ delay: number; release: () => void }> = [];
const pull = new SerialVideoPull<number>((delay) => new Promise((resolve) => {
  waits.push({ delay, release: resolve });
}));
```

- [ ] **Step 2: Verify RED**

Run `bun test src/video-pull-loop.test.ts`. Expected: missing retry export/callback behavior.

- [ ] **Step 3: Implement minimal retry behavior**

Use an injected wait function for deterministic tests. Check generation/key before and after the delay. Do not retry delivery callback exceptions; retain the existing delivery error path.

```ts
export function videoPullRetryDelay(consecutiveFailures: number): number {
  return [100, 250, 500, 1_000, 2_000][Math.min(4, Math.max(0, consecutiveFailures - 1))]!;
}

type PullErrorHandler = (
  error: unknown,
  consecutiveFailures: number,
  retryDelayMs: number,
) => void;
```

- [ ] **Step 4: Integrate the cumulative frontend counter**

Increment `videoPullFailures` in `onPullError`, reset it with video telemetry, include it in `reportControllerRenderMetrics`, and reject values above `1_000_000` at the Rust command boundary.

```ts
videoPull.start(
  { streamId: config.streamId, configVersion: config.configVersion },
  nextControllerVideoFrame,
  handleVideo,
  handleVideoDeliveryError,
  () => {
    videoPullFailures = Math.min(1_000_000, videoPullFailures + 1);
  },
);
```

```rust
if metrics.video_pull_failures > MAX_CONTROLLER_VIDEO_PULL_FAILURES {
    return Err("远程画面拉取指标无效。".to_owned());
}
```

- [ ] **Step 5: Verify GREEN**

Run the focused test, `bunx tsc --noEmit`, and the focused Rust render-metrics tests.

### Task 2: Mailbox Delivery Metrics

**Files:**
- Modify: `apps/windows-ui/src-tauri/src/video_mailbox.rs`
- Modify: `apps/windows-ui/src-tauri/src/controller.rs`

**Interfaces:**
- Produces: `VideoMailboxMetrics { delivered_frames, overflow_drops, keyframe_replacements }`.
- Produces: `ControllerVideoMailbox::{metrics, reset_metrics}`.

- [ ] **Step 1: Write failing metric tests**

Prove exact increments for successful delivery, delta overflow, keyframe replacement, wrong-key ignore, close preservation, and explicit reset.

```rust
assert_eq!(mailbox.offer(frame(key, 10, true), now), VideoMailboxOffer::Queued);
assert_eq!(mailbox.next(key).await.unwrap().frame_id, 10);
assert_eq!(mailbox.metrics().delivered_frames, 1);

assert_eq!(mailbox.offer(frame(key, 11, true), now), VideoMailboxOffer::Queued);
assert_eq!(mailbox.offer(frame(key, 12, false), now), VideoMailboxOffer::RequestKeyframe);
assert_eq!(mailbox.metrics().overflow_drops, 1);
```

- [ ] **Step 2: Verify RED**

Run `cargo test -p desklink-windows-ui --lib video_mailbox -- --nocapture`. Expected: missing metrics types/methods.

- [ ] **Step 3: Implement counters in mailbox state**

Use `saturating_add(1)` under the existing mutex. `begin_config` and `close` preserve counters; `reset_metrics` clears only counters.

```rust
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct VideoMailboxMetrics {
    pub(crate) delivered_frames: u64,
    pub(crate) overflow_drops: u64,
    pub(crate) keyframe_replacements: u64,
}
```

- [ ] **Step 4: Wire per-attempt reset and diagnostic sampling**

Reset after each attempt begins. Add the three fields to ten-second video diagnostics and record a final sample immediately after the attempt loop exits.

```rust
manager.video_mailbox.close();
manager.video_mailbox.reset_metrics();

record_controller_video_metrics(
    diagnostics.as_ref(),
    attempt,
    runtime.metrics(),
    manager.video_mailbox.metrics(),
    manager.input_backpressure_count.load(Ordering::Relaxed),
);
```

- [ ] **Step 5: Verify GREEN**

Run all Windows UI Rust tests and formatting.

### Task 3: Local Diagnostic Contract

**Files:**
- Modify: `apps/windows/src/diagnostics.rs`
- Modify: `apps/windows-ui/src-tauri/src/controller.rs`

**Interfaces:**
- Extends: `DiagnosticEvent::ControllerVideoMetrics` with three mailbox counters.
- Extends: `DiagnosticEvent::ControllerRenderMetrics` with `video_pull_failures`.

- [ ] **Step 1: Write failing serialization tests**

Update the existing video and render metric fixtures with known values and assert all four new JSON fields.

```rust
assert!(contents.contains("\"delivered_video_frames\":84"));
assert!(contents.contains("\"video_ipc_overflow_drops\":3"));
assert!(contents.contains("\"video_ipc_keyframe_replacements\":1"));
assert!(contents.contains("\"video_pull_failures\":2"));
```

- [ ] **Step 2: Verify RED**

Run `cargo test -p desklink-windows diagnostics::tests::controller_ -- --nocapture`. Expected: missing enum fields or missing JSON assertions.

- [ ] **Step 3: Extend the diagnostic enum and encoder**

Serialize only unsigned bounded counters; keep schema `1` and existing event names.

```rust
ControllerVideoMetrics {
    attempt: u32,
    received_video_packets: u64,
    dropped_video_packets: u64,
    completed_frames: u64,
    delivered_video_frames: u64,
    video_ipc_overflow_drops: u64,
    video_ipc_keyframe_replacements: u64,
    input_backpressure_count: u64,
}
```

- [ ] **Step 4: Refactor repeated video logging into one helper**

Use the same helper for periodic and final attempt samples so field meanings cannot drift.

```rust
fn record_controller_video_metrics(
    diagnostics: Option<&DiagnosticLog>,
    attempt: u32,
    transport: ControllerMetrics,
    mailbox: VideoMailboxMetrics,
    input_backpressure_count: u64,
) {
    // Construct and record exactly one DiagnosticEvent::ControllerVideoMetrics.
}
```

- [ ] **Step 5: Verify GREEN**

Run `cargo test -p desklink-windows` and `cargo test -p desklink-windows-ui --lib`.

### Task 4: Cloud Validation and Automated Findings

**Files:**
- Modify: `server/diagnostics/src/validation.ts`
- Modify: `server/diagnostics/src/diagnostics.test.ts`
- Modify: `server/diagnostics/src/analysis.ts`
- Modify: `server/diagnostics/src/analysis.test.ts`

**Interfaces:**
- Accepts fields: `delivered_video_frames`, `video_ipc_overflow_drops`, `video_ipc_keyframe_replacements`, `video_pull_failures`.
- Produces findings: `video_ipc_stalled`, `video_ipc_pressure`, `video_pull_instability`.

- [ ] **Step 1: Write failing signed-ingestion test**

Add all four fields to a signed fixture and assert the verified events preserve them. Expected RED: unsupported fields.

```ts
events: [
  eventFixture({
    event: "controller_video_metrics",
    delivered_video_frames: 84,
    video_ipc_overflow_drops: 3,
    video_ipc_keyframe_replacements: 1,
  }),
  eventFixture({
    event: "controller_render_metrics",
    video_pull_failures: 2,
  }),
]
```

- [ ] **Step 2: Write failing analysis tests**

Test zero delivered frames with completed frames, thresholds at two versus three, pull failure threshold, legacy events without mailbox fields, and healthy suppression.

```ts
expect(session.findings.map((finding) => finding.code)).toContain("video_ipc_stalled");
expect(twoOverflows.findings.map((finding) => finding.code)).not.toContain("video_ipc_pressure");
expect(threeOverflows.findings.map((finding) => finding.code)).toContain("video_ipc_pressure");
expect(threePullFailures.findings.map((finding) => finding.code)).toContain("video_pull_instability");
```

- [ ] **Step 3: Implement whitelist and aggregation**

Track whether any mailbox sample exists. Aggregate cumulative fields with `max` inside an attempt and sum across attempts/streams, matching existing semantics.

```ts
interface VideoAttempt {
  received: number;
  dropped: number;
  completed: number;
  delivered: number;
  ipcOverflowDrops: number;
  ipcKeyframeReplacements: number;
  mailboxSamples: number;
  inputBackpressure: number;
}
```

- [ ] **Step 4: Implement exact findings**

Use threshold `3` for IPC overflow and pull failures. Do not warn for one or two events and do not classify legacy missing fields as stalled.

```ts
const VIDEO_IPC_PRESSURE_WARNING = 3;
const VIDEO_PULL_FAILURE_WARNING = 3;

if (video.completed > 0 && video.mailboxSamples > 0 && video.delivered === 0) {
  findings.push({ code: "video_ipc_stalled", severity: "error", title: "视频没有交付到界面", detail: "..." });
}
```

- [ ] **Step 5: Verify GREEN**

Run `bun test` and `bunx tsc --noEmit` in `server/diagnostics`.

### Task 5: Version, Documentation, and Release Verification

**Files:**
- Modify: the five Windows version sources and three matching workspace packages in `Cargo.lock`
- Modify: `README.md`
- Modify: `docs/cloud-diagnostics.md`
- Modify: `docs/windows-architecture-review.md`
- Create: `docs/windows-0.1.60-video-delivery-self-healing.md`
- Generate ignored artifact: `dist/windows/DeskLinkSetup-0.1.60-x64.exe`

**Interfaces:**
- Produces: Windows `0.1.60`, unchanged protocol `9`, unsigned x64 test installer.

- [ ] **Step 1: Bump Windows versions only**

Change `0.1.59` to `0.1.60` in the five product sources and the three matching workspace lock packages. Do not change the registry crate `cmake` version.

```text
apps/windows/Cargo.toml
apps/windows-ui/src-tauri/Cargo.toml
apps/windows-ui/package.json
apps/windows-ui/src-tauri/tauri.conf.json
tools/windows-installer/Cargo.toml
Cargo.lock: desklink-installer, desklink-windows, desklink-windows-ui
```

- [ ] **Step 2: Document privacy, thresholds, and deployment order**

Record that the diagnostics service must be deployed from the later clean commit before distributing 0.1.60 with diagnostics sharing enabled.

- [ ] **Step 3: Run all quality gates**

Run Rust fmt/workspace tests/Clippy, Python tests, both Bun workspaces, and both production builds.

```powershell
cargo fmt --all -- --check
python -X utf8 scripts/run-windows-cargo.py test --workspace --all-targets --all-features --jobs 1
python -X utf8 scripts/run-windows-cargo.py clippy --workspace --all-targets --all-features -- -D warnings
python -X utf8 -m unittest discover -s scripts/tests -p test_*.py
cd apps/windows-ui; bun install --frozen-lockfile; bun test; bun run build
cd ../../server/diagnostics; bun install --frozen-lockfile; bun run check
```

- [ ] **Step 4: Probe protocol 9 and build installer**

Run `python -X utf8 scripts/verify-managed-relay.py` and `python -X utf8 scripts/build-windows-installer.py`.

Expected installer: `dist/windows/DeskLinkSetup-0.1.60-x64.exe`; both JSON manifests must report `passed: true`.

- [ ] **Step 5: Audit scope and hashes**

Confirm both manifests pass, independently compare SHA-256, verify protocol 9, no macOS diff, main branch, and `git diff --check`. Keep changes uncommitted and unpushed.
