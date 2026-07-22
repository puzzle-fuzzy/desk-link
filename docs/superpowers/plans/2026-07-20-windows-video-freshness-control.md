# Windows Video Freshness Control Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Windows automatic video quality react to controller decode backlog and recover to a fresh keyframe instead of silently dropping dependent H.264 frames.

**Architecture:** A pure TypeScript pressure state machine samples WebCodecs queue depth and triggers bounded freshness recovery. Tauri stores a bounded, stream-scoped sample and attaches it to the controller runtime's existing encrypted network feedback; protocol version 9 carries the extra fields, and the Windows host folds them into the existing hysteretic quality policy.

**Tech Stack:** TypeScript, Bun, WebCodecs, Tauri 2, Rust, Tokio, Serde/Postcard, Noise-encrypted QUIC control channel, Cargo workspace tests.

## Global Constraints

- Windows 10/11 x64 only; do not add or change macOS code.
- All user-visible copy remains Chinese and all icons remain Lucide-owned.
- Do not change pairing, approval, input, clipboard, file, audio, or relay service responsibilities.
- Protocol version changes from 8 to 9; both Windows computers must upgrade together.
- Relay remains an opaque forwarder and does not require deployment.
- Playback feedback fields are bounded to queue depth `0..=64` and recoveries `0..=16` per sample.
- Manual quality modes ignore automatic pressure feedback.
- Do not add playback buffers or reduce any quality preset below 30 FPS.
- Do not commit or push until the user explicitly requests it.

---

### Task 1: Pure WebCodecs pressure policy

**Files:**
- Create: `apps/windows-ui/src/video-playback-pressure.ts`
- Create: `apps/windows-ui/src/video-playback-pressure.test.ts`

**Interfaces:**
- Produces: `VideoPlaybackPressure.observe(queueSize: number, nowMs: number): "submit" | "recover"`.
- Produces: `VideoPlaybackPressure.takeSample(): VideoPlaybackPressureSample`.
- Produces: `VideoPlaybackPressure.reset(): void`.
- Produces: `VideoPlaybackPressureSample { peakDecodeQueueSize: number; freshnessRecoveries: number }`.

- [ ] **Step 1: Write failing state-machine tests**

```ts
import { describe, expect, test } from "bun:test";
import {
  VIDEO_FRESHNESS_COOLDOWN_MS,
  VideoPlaybackPressure,
} from "./video-playback-pressure";

describe("远程视频播放压力", () => {
  test("连续三次严重积压才恢复到新关键帧", () => {
    const pressure = new VideoPlaybackPressure();
    expect(pressure.observe(5, 1_000)).toBe("submit");
    expect(pressure.observe(6, 1_010)).toBe("submit");
    expect(pressure.observe(7, 1_020)).toBe("recover");
  });

  test("健康队列会打断严重积压计数", () => {
    const pressure = new VideoPlaybackPressure();
    pressure.observe(5, 1_000);
    pressure.observe(1, 1_010);
    pressure.observe(5, 1_020);
    pressure.observe(5, 1_030);
    expect(pressure.observe(5, 1_040)).toBe("recover");
  });

  test("恢复之间保留五秒冷却", () => {
    const pressure = new VideoPlaybackPressure();
    pressure.observe(5, 1_000);
    pressure.observe(5, 1_010);
    expect(pressure.observe(5, 1_020)).toBe("recover");
    pressure.observe(5, 1_030);
    pressure.observe(5, 1_040);
    expect(pressure.observe(5, 1_050)).toBe("submit");
    pressure.observe(5, 1_020 + VIDEO_FRESHNESS_COOLDOWN_MS);
    pressure.observe(5, 1_030 + VIDEO_FRESHNESS_COOLDOWN_MS);
    expect(pressure.observe(5, 1_040 + VIDEO_FRESHNESS_COOLDOWN_MS)).toBe("recover");
  });

  test("取出样本清零周期峰值但保留冷却状态", () => {
    const pressure = new VideoPlaybackPressure();
    pressure.observe(5, 1_000);
    pressure.observe(6, 1_010);
    pressure.observe(7, 1_020);
    expect(pressure.takeSample()).toEqual({ peakDecodeQueueSize: 7, freshnessRecoveries: 1 });
    expect(pressure.takeSample()).toEqual({ peakDecodeQueueSize: 0, freshnessRecoveries: 0 });
  });
});
```

- [ ] **Step 2: Run the focused Bun test and confirm RED**

Run: `bun test src/video-playback-pressure.test.ts`

Expected: FAIL because `video-playback-pressure.ts` does not exist.

- [ ] **Step 3: Implement the minimal bounded pressure state**

```ts
export const VIDEO_QUEUE_OVERLOAD_THRESHOLD = 5;
export const VIDEO_QUEUE_OVERLOAD_SAMPLES = 3;
export const VIDEO_FRESHNESS_COOLDOWN_MS = 5_000;
export const MAX_REPORTED_DECODE_QUEUE_SIZE = 64;
export const MAX_REPORTED_FRESHNESS_RECOVERIES = 16;

export type VideoPlaybackPressureSample = {
  peakDecodeQueueSize: number;
  freshnessRecoveries: number;
};

export class VideoPlaybackPressure {
  private peakDecodeQueueSize = 0;
  private severeSamples = 0;
  private freshnessRecoveries = 0;
  private lastRecoveryAtMs: number | null = null;

  observe(queueSize: number, nowMs: number): "submit" | "recover" {
    const boundedQueue = Math.min(
      MAX_REPORTED_DECODE_QUEUE_SIZE,
      Math.max(0, Number.isFinite(queueSize) ? Math.trunc(queueSize) : 0),
    );
    this.peakDecodeQueueSize = Math.max(this.peakDecodeQueueSize, boundedQueue);
    if (boundedQueue < VIDEO_QUEUE_OVERLOAD_THRESHOLD) {
      this.severeSamples = 0;
      return "submit";
    }
    this.severeSamples = Math.min(VIDEO_QUEUE_OVERLOAD_SAMPLES, this.severeSamples + 1);
    if (this.severeSamples < VIDEO_QUEUE_OVERLOAD_SAMPLES) {
      return "submit";
    }
    this.severeSamples = 0;
    if (
      this.lastRecoveryAtMs !== null
      && nowMs - this.lastRecoveryAtMs < VIDEO_FRESHNESS_COOLDOWN_MS
    ) {
      return "submit";
    }
    this.lastRecoveryAtMs = nowMs;
    this.freshnessRecoveries = Math.min(
      MAX_REPORTED_FRESHNESS_RECOVERIES,
      this.freshnessRecoveries + 1,
    );
    return "recover";
  }

  takeSample(): VideoPlaybackPressureSample {
    const sample = {
      peakDecodeQueueSize: this.peakDecodeQueueSize,
      freshnessRecoveries: this.freshnessRecoveries,
    };
    this.peakDecodeQueueSize = 0;
    this.freshnessRecoveries = 0;
    return sample;
  }

  reset(): void {
    this.peakDecodeQueueSize = 0;
    this.severeSamples = 0;
    this.freshnessRecoveries = 0;
    this.lastRecoveryAtMs = null;
  }
}
```

- [ ] **Step 4: Run focused and full Bun tests**

Run: `bun test src/video-playback-pressure.test.ts`

Expected: 4 pass, 0 fail.

Run: `bun test`

Expected: all existing 101 tests plus the new tests pass.

---

### Task 2: Protocol 9 encrypted playback-pressure fields

**Files:**
- Modify: `crates/desklink-protocol/src/lib.rs`
- Modify: `crates/desklink-protocol/tests/round_trip.rs`
- Modify: `crates/desklink-ffi/src/controller.rs`

**Interfaces:**
- Changes: `PROTOCOL_VERSION: u16 = 9`.
- Changes: `ControlMessage::VideoNetworkFeedback { received_packets, dropped_packets, decode_queue_peak, freshness_recoveries }`.
- Changes: `ControllerRuntime::report_video_network_feedback(received_packets, dropped_packets, decode_queue_peak, freshness_recoveries)`.

- [ ] **Step 1: Extend the protocol round-trip expectation first**

Update the existing `video_quality_commands_round_trip` test message to:

```rust
ControlMessage::VideoNetworkFeedback {
    received_packets: 120,
    dropped_packets: 3,
    decode_queue_peak: 7,
    freshness_recoveries: 1,
},
```

Add:

```rust
#[test]
fn playback_pressure_requires_protocol_nine() {
    assert_eq!(PROTOCOL_VERSION, 9);
}
```

- [ ] **Step 2: Run the focused protocol test and confirm RED**

Run: `python -X utf8 scripts/run-windows-cargo.py test -p desklink-protocol video_quality_commands_round_trip --jobs 1`

Expected: compile failure because the feedback variant has no playback-pressure fields.

- [ ] **Step 3: Add the fields and update the encrypted controller sender**

Set `PROTOCOL_VERSION` to 9, add both `u16` fields to the enum variant, and extend `report_video_network_feedback` to encode them. Update every exhaustive construction or pattern match reported by the compiler without changing other message semantics.

- [ ] **Step 4: Verify protocol and FFI tests**

Run: `python -X utf8 scripts/run-windows-cargo.py test -p desklink-protocol -p desklink-ffi --all-targets --jobs 1`

Expected: all protocol and FFI tests pass.

---

### Task 3: Stream-scoped Tauri pressure boundary

**Files:**
- Modify: `apps/windows-ui/src/types.ts`
- Modify: `apps/windows-ui/src/api.ts`
- Modify: `apps/windows-ui/src-tauri/src/controller.rs`
- Modify: `apps/windows-ui/src-tauri/src/lib.rs`

**Interfaces:**
- Produces TS `ControllerPlaybackPressure { streamId, peakDecodeQueueSize, freshnessRecoveries }`.
- Produces API `reportControllerPlaybackPressure(pressure): Promise<void>`.
- Produces Rust `ControllerPlaybackPressure` with camelCase deserialization.
- Produces manager methods `record_playback_pressure` and `take_playback_pressure`.

- [ ] **Step 1: Add failing Rust manager tests**

In the existing `controller.rs` test module add tests that create a default manager and connected stream status, then assert:

```rust
manager.record_playback_pressure(ControllerPlaybackPressure {
    stream_id: 9,
    peak_decode_queue_size: 5,
    freshness_recoveries: 1,
}).unwrap();
manager.record_playback_pressure(ControllerPlaybackPressure {
    stream_id: 9,
    peak_decode_queue_size: 7,
    freshness_recoveries: 2,
}).unwrap();
assert_eq!(
    manager.take_playback_pressure(9),
    PlaybackPressureSample { decode_queue_peak: 7, freshness_recoveries: 3 },
);
assert_eq!(manager.take_playback_pressure(9), PlaybackPressureSample::default());
```

Add separate assertions that stream 8 is ignored while stream 9 is active, queue 65 is rejected, and recoveries 17 is rejected.

- [ ] **Step 2: Run the focused Tauri test and confirm RED**

Run: `python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows-ui playback_pressure --jobs 1`

Expected: compile failure because the pressure types and manager methods do not exist.

- [ ] **Step 3: Implement bounded merge-and-take storage**

Add `playback_pressure: Arc<Mutex<Option<ControllerPlaybackPressure>>>` to `ControllerManager`. Validate against constants 64 and 16, ignore stale stream IDs, merge queue by maximum and recoveries with saturation, clear the field when starting a new controller operation, and have `take_playback_pressure(stream_id)` remove stale data and return a default sample when empty.

Add this Tauri command and register it:

```rust
#[tauri::command]
fn report_controller_playback_pressure(
    manager: State<'_, ControllerManager>,
    pressure: ControllerPlaybackPressure,
) -> Result<(), String> {
    manager.record_playback_pressure(pressure)
}
```

Add the matching TypeScript interface and API wrapper.

- [ ] **Step 4: Attach the sample to the existing one-second feedback**

In the `last_metrics.elapsed() >= Duration::from_secs(1)` block, call `manager.take_playback_pressure(stream_id)` and pass both bounded fields to `runtime.report_video_network_feedback`. Continue sending feedback only in automatic mode; taking the sample in manual mode must still clear it so stale pressure cannot affect a later mode change.

- [ ] **Step 5: Verify Tauri tests and frontend type checking**

Run: `python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows-ui playback_pressure --jobs 1`

Expected: all playback pressure tests pass.

Run: `bun run build` from `apps/windows-ui`.

Expected: TypeScript and Vite production build pass.

---

### Task 4: Host automatic-quality pressure policy

**Files:**
- Modify: `apps/windows/src/runtime.rs`

**Interfaces:**
- Changes: `AdaptiveVideoQuality::observe(received_packets, dropped_packets, decode_queue_peak, freshness_recoveries)`.
- Consumes: protocol 9 feedback fields from Task 2.

- [ ] **Step 1: Write failing host policy tests**

Add or update focused tests to prove:

```rust
let mut quality = AdaptiveVideoQuality::new();
assert_eq!(quality.observe(0, 0, 7, 1), Some(VideoQualityPreset::Balanced));
quality.record_applied(VideoQualityPreset::Balanced);

let mut quality = AdaptiveVideoQuality::new();
assert_eq!(quality.observe(0, 0, 5, 0), None);
assert_eq!(quality.observe(0, 0, 5, 0), Some(VideoQualityPreset::Balanced));

let mut quality = AdaptiveVideoQuality::new();
quality.set_preference(VideoQualityPreference::Sharp);
assert_eq!(quality.observe(0, 0, 64, 16), None);
```

Update the existing healthy recovery test so all 12 samples use queue peak 2 and zero recoveries. Add a test that queue peak 3 prevents a healthy upgrade.

- [ ] **Step 2: Run focused tests and confirm RED**

Run: `python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows automatic_quality --jobs 1`

Expected: compile failure because `observe` still accepts two arguments.

- [ ] **Step 3: Implement combined loss and playback-pressure classification**

Apply this order:

1. Return immediately for manual preference.
2. Consume one cooldown sample and return if cooling down.
3. Treat `freshness_recoveries > 0` like severe loss and request one lower preset immediately.
4. Compute loss only when at least 40 packets exist.
5. Count queue peak at least 5 or loss at least 5% as a degraded sample.
6. Count health only with at least 40 packets, loss at most 0.5%, queue peak at most 2, and zero recoveries.
7. Reset both counters for ambiguous samples.

Update the host control match to pass all four fields.

- [ ] **Step 4: Verify host policy and workspace protocol integration**

Run: `python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows automatic_quality --jobs 1`

Expected: all automatic-quality tests pass.

Run: `python -X utf8 scripts/run-windows-cargo.py test -p desklink-end-to-end -p desklink-transport --all-targets --jobs 1`

Expected: protocol-9 directory and recovery tests pass without changing relay behavior.

---

### Task 5: Frontend freshness recovery integration

**Files:**
- Modify: `apps/windows-ui/src/controller.ts`
- Test: `apps/windows-ui/src/video-playback-pressure.test.ts`

**Interfaces:**
- Consumes: `VideoPlaybackPressure` from Task 1.
- Consumes: `reportControllerPlaybackPressure` from Task 3.
- Produces: local decoder restart that preserves the last Canvas frame and requests a keyframe.

- [ ] **Step 1: Add edge-case pressure tests before integration**

Add tests that non-finite/negative queue sizes normalize to zero, values above 64 report 64, and `reset()` clears both a pending sample and cooldown so the next stream starts clean. Run the focused test and confirm these cases fail before updating the state module.

- [ ] **Step 2: Implement the edge cases and re-run focused tests**

Run: `bun test src/video-playback-pressure.test.ts`.

Expected: all pressure tests pass.

- [ ] **Step 3: Replace arbitrary frame dropping with recovery decision**

Create one module-level `VideoPlaybackPressure`. In `submitVideoChunk`, observe `decoder.decodeQueueSize` before decode. Remove:

```ts
if (!keyframe && decoder.decodeQueueSize > 4) {
  return;
}
```

When `observe` returns `recover`, schedule a freshness restart using the current config key and decoder preference, return without submitting that frame, and let `startVideoDecoder` request the new keyframe. Do not increment `decoderRecoveries`, because the decoder did not fail. Do not clear the Canvas.

- [ ] **Step 4: Add the one-second bounded IPC reporter**

Use one recursive timeout while a video config is active. Each tick calls `takeSample`; send only a non-zero sample through `reportControllerPlaybackPressure`, ignore IPC rejection, and re-arm only if the same stream remains active. `resetVideoTelemetry` must clear the timer and call `pressure.reset()` so a late old-stream tick cannot affect a new stream.

- [ ] **Step 5: Verify frontend tests and production build**

Run: `bun test` from `apps/windows-ui`.

Expected: all previous and new tests pass.

Run: `bun run build` from `apps/windows-ui`.

Expected: TypeScript type checking and Vite production build pass with no handwritten SVG.

---

### Task 6: Version 0.1.57, documentation, and release gates

**Files:**
- Modify: `apps/windows/Cargo.toml`
- Modify: `apps/windows-ui/src-tauri/Cargo.toml`
- Modify: `apps/windows-ui/package.json`
- Modify: `apps/windows-ui/src-tauri/tauri.conf.json`
- Modify: `tools/windows-installer/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `README.md`
- Modify: `docs/windows-architecture-review.md`
- Create: `docs/windows-0.1.57-video-freshness-control.md`

**Interfaces:**
- Produces: version-consistent unsigned Windows 0.1.57 test installer.

- [ ] **Step 1: Bump only DeskLink package/config versions**

Change the five package/config versions and three matching workspace package entries in `Cargo.lock` from `0.1.56` to `0.1.57`. Leave historical release documents unchanged.

- [ ] **Step 2: Document user-visible behavior and compatibility**

Document that automatic mode reacts to sustained decoder pressure, severe backlog reanchors at a new keyframe without clearing the last Canvas frame, protocol 9 requires both computers to upgrade, and the relay server does not require deployment. Keep technical queue numbers out of the ordinary toolbar instructions.

- [ ] **Step 3: Run complete verification**

Run:

```powershell
cargo fmt --all -- --check
cd apps/windows-ui
bun install --frozen-lockfile
bun test
bun run build
cd ../..
python -X utf8 -m unittest discover -s scripts/tests -p "test_*.py" -v
python -X utf8 scripts/run-windows-cargo.py test --workspace --all-targets --all-features --jobs 1
python -X utf8 scripts/run-windows-cargo.py clippy --workspace --all-targets --all-features --jobs 1 -- -D warnings
python -X utf8 scripts/verify-managed-relay.py
python -X utf8 scripts/build-windows-installer.py
git diff --check
```

Expected: Bun, Python, Rust, Clippy, production relay probe, release verification, and installer build all pass.

- [ ] **Step 4: Verify the final artifact**

Confirm `dist/windows/DeskLinkSetup-0.1.57-x64.exe` exists, its SHA-256 matches `dist/windows/windows-installer-manifest.json`, the manifest has `passed: true`, `version: "0.1.57"`, and `signed: false` unless a signing identity has since been configured.
