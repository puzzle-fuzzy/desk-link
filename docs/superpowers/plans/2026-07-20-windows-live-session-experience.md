# Windows Live Session Experience Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship DeskLink Windows 0.1.56 with automatic 30 FPS quality as the default, a user-facing session summary instead of technical counters, and an auto-hiding native-fullscreen toolbar.

**Architecture:** Keep the existing Rust host adaptation state machine and Tauri fullscreen boundary. Add one pure TypeScript presentation module for toolbar visibility and status copy; `controller.ts` translates DOM events into that module without rerendering the video surface or adding network traffic.

**Tech Stack:** Rust, Tokio, Media Foundation H.264, Tauri 2, Vanilla TypeScript, CSS, Bun test, Cargo test/Clippy.

## Global Constraints

- Windows-only implementation; do not modify macOS code.
- Keep DeskLink protocol version 8 and do not deploy the relay.
- Keep all visible UI copy in Chinese and all icons from Lucide.
- Do not expose technical frame or packet counters in the primary remote-session toolbar.
- Do not remove detailed local/cloud diagnostic metrics.
- Preserve the current contain scaling, multi-display switching, encrypted transport, input release, file transfer and clipboard behavior.
- Do not commit or push unless the user explicitly requests it.

---

### Task 1: Make Automatic 30 FPS the Host Default

**Files:**
- Modify: `apps/windows/src/runtime.rs`

**Interfaces:**
- Consumes: existing `VideoQualityPreference`, `VideoQualityPreset`, `H264EncoderSettings`, and `AdaptiveVideoQuality`.
- Produces: `DEFAULT_VIDEO_QUALITY_PREFERENCE: VideoQualityPreference` and unchanged `video_quality_settings(VideoQualityPreset) -> H264EncoderSettings` with 30 FPS for every preset.

- [ ] **Step 1: Write failing Rust tests for the new default and frame rates**

Replace the current profile assertion and add a default-policy assertion inside `runtime.rs` tests:

```rust
#[test]
fn quality_profiles_preserve_thirty_fps_while_reducing_bandwidth() {
    let smooth = video_quality_settings(VideoQualityPreset::Smooth);
    let balanced = video_quality_settings(VideoQualityPreset::Balanced);
    let sharp = video_quality_settings(VideoQualityPreset::Sharp);
    assert_eq!((smooth.max_width, smooth.max_height, smooth.fps), (1280, 720, 30));
    assert_eq!((balanced.max_width, balanced.max_height, balanced.fps), (1920, 1080, 30));
    assert_eq!(sharp.fps, 30);
    assert!(smooth.bitrate < balanced.bitrate);
    assert!(balanced.bitrate < sharp.bitrate);
}

#[test]
fn automatic_quality_is_the_default_session_policy() {
    let quality = AdaptiveVideoQuality::new();
    assert_eq!(quality.preference, VideoQualityPreference::Automatic);
    assert_eq!(quality.preset, VideoQualityPreset::Sharp);
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```powershell
python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows --lib quality_profiles_preserve_thirty_fps_while_reducing_bandwidth
python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows --lib automatic_quality_is_the_default_session_policy
```

Expected: the first test fails because Smooth/Balanced are 15/20 FPS; the second fails because the preference is `Sharp`.

- [ ] **Step 3: Implement the minimal host policy change**

Add and use the default preference constant, preserve the current bitrate values, and change only FPS values:

```rust
const DEFAULT_VIDEO_QUALITY: VideoQualityPreset = VideoQualityPreset::Sharp;
const DEFAULT_VIDEO_QUALITY_PREFERENCE: VideoQualityPreference =
    VideoQualityPreference::Automatic;

// Smooth settings
fps: 30,
bitrate: 1_500_000,

// Balanced settings
fps: 30,
bitrate: 2_500_000,
```

Initialize `AdaptiveVideoQuality.preference` with `DEFAULT_VIDEO_QUALITY_PREFERENCE`, and send that preference in the initial `send_video_quality_state` call instead of a hard-coded `Sharp`.

- [ ] **Step 4: Run the focused and neighboring adaptation tests**

Run:

```powershell
python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows --lib quality_profiles -- --nocapture
python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows --lib automatic_quality -- --nocapture
python -X utf8 scripts/run-windows-cargo.py test -p desklink-windows --lib frame_pacer -- --nocapture
```

Expected: all matching tests pass, including sustained-loss downgrade, long healthy recovery, manual-mode suppression, and 60 Hz pacing.

- [ ] **Step 5: Review the task diff without committing**

Run `git diff --check` and `git diff -- apps/windows/src/runtime.rs`. Confirm no protocol or relay files changed.

---

### Task 2: Add Pure Session Presentation Rules

**Files:**
- Create: `apps/windows-ui/src/remote-session-presentation.ts`
- Create: `apps/windows-ui/src/remote-session-presentation.test.ts`

**Interfaces:**
- Consumes: `VideoQualityPreference` and `VideoQualityPreset` from `types.ts`.
- Produces: `REMOTE_TOOLBAR_IDLE_MS`, `RemoteToolbarVisibilityInput`, `remoteToolbarVisible`, `remoteToolbarHideDelay`, and `remoteSessionSummary`.

- [ ] **Step 1: Write the failing Bun tests**

Create `remote-session-presentation.test.ts`:

```typescript
import { describe, expect, test } from "bun:test";
import {
  REMOTE_TOOLBAR_IDLE_MS,
  remoteSessionSummary,
  remoteToolbarHideDelay,
  remoteToolbarVisible,
} from "./remote-session-presentation";

const base = {
  connected: true,
  fullscreen: true,
  nowMs: 4_000,
  lastRevealedAtMs: 1_000,
  pointerNearTop: false,
  toolbarFocused: false,
  panelOpen: false,
};

describe("远程会话工具栏", () => {
  test("普通窗口和非连接状态始终保持可见", () => {
    expect(remoteToolbarVisible({ ...base, fullscreen: false })).toBe(true);
    expect(remoteToolbarVisible({ ...base, connected: false })).toBe(true);
  });

  test("全屏显示三秒后收起", () => {
    expect(REMOTE_TOOLBAR_IDLE_MS).toBe(3_000);
    expect(remoteToolbarVisible({ ...base, nowMs: 3_999 })).toBe(true);
    expect(remoteToolbarVisible(base)).toBe(false);
    expect(remoteToolbarHideDelay({ ...base, nowMs: 2_250 })).toBe(1_750);
  });

  test("顶部触发、工具栏焦点和面板打开都会固定显示", () => {
    expect(remoteToolbarVisible({ ...base, pointerNearTop: true })).toBe(true);
    expect(remoteToolbarVisible({ ...base, toolbarFocused: true })).toBe(true);
    expect(remoteToolbarVisible({ ...base, panelOpen: true })).toBe(true);
    expect(remoteToolbarHideDelay({ ...base, panelOpen: true })).toBeNull();
  });
});

describe("远程会话状态摘要", () => {
  test("自动画质只显示用户需要的当前档位", () => {
    expect(remoteSessionSummary(1920, 1080, "automatic", "sharp"))
      .toBe("1920 × 1080 · 自动（清晰） · 已加密");
    expect(remoteSessionSummary(1280, 720, "automatic", "smooth"))
      .toBe("1280 × 720 · 自动（流畅） · 已加密");
  });

  test("手动画质不伪装成自动状态", () => {
    expect(remoteSessionSummary(1920, 1080, "balanced", "balanced"))
      .toBe("1920 × 1080 · 均衡 · 已加密");
  });
});
```

- [ ] **Step 2: Run the test and verify RED**

Run `bun test src/remote-session-presentation.test.ts` from `apps/windows-ui`.

Expected: FAIL because `remote-session-presentation.ts` does not exist.

- [ ] **Step 3: Implement the pure module**

Create `remote-session-presentation.ts`:

```typescript
import type { VideoQualityPreference, VideoQualityPreset } from "./types";

export const REMOTE_TOOLBAR_IDLE_MS = 3_000;

export type RemoteToolbarVisibilityInput = {
  connected: boolean;
  fullscreen: boolean;
  nowMs: number;
  lastRevealedAtMs: number;
  pointerNearTop: boolean;
  toolbarFocused: boolean;
  panelOpen: boolean;
};

export function remoteToolbarVisible(input: RemoteToolbarVisibilityInput): boolean {
  if (!input.connected || !input.fullscreen) return true;
  if (input.pointerNearTop || input.toolbarFocused || input.panelOpen) return true;
  return input.nowMs - input.lastRevealedAtMs < REMOTE_TOOLBAR_IDLE_MS;
}

export function remoteToolbarHideDelay(input: RemoteToolbarVisibilityInput): number | null {
  if (!input.connected || !input.fullscreen) return null;
  if (input.pointerNearTop || input.toolbarFocused || input.panelOpen) return null;
  return Math.max(0, REMOTE_TOOLBAR_IDLE_MS - (input.nowMs - input.lastRevealedAtMs));
}

function presetLabel(preset: VideoQualityPreset): string {
  return preset === "smooth" ? "流畅" : preset === "balanced" ? "均衡" : "清晰";
}

export function remoteSessionSummary(
  width: number,
  height: number,
  preference: VideoQualityPreference,
  preset: VideoQualityPreset,
): string {
  const quality = preference === "automatic"
    ? `自动（${presetLabel(preset)}）`
    : presetLabel(preference);
  return `${width} × ${height} · ${quality} · 已加密`;
}
```

- [ ] **Step 4: Run focused and complete Bun tests**

Run:

```powershell
bun test src/remote-session-presentation.test.ts
bun test
```

Expected: the focused tests and the full suite pass without warnings.

- [ ] **Step 5: Review the task diff without committing**

Run `git diff --check` and inspect both new files. Confirm the module has no DOM, storage or IPC dependency.

---

### Task 3: Integrate the Immersive Toolbar Without Rerendering Video

**Files:**
- Modify: `apps/windows-ui/src/controller.ts`
- Modify: `apps/windows-ui/src/styles.css`

**Interfaces:**
- Consumes: the five exports from `remote-session-presentation.ts`.
- Produces: `.remote-session[data-remote-toolbar-visible="true|false"]`, a top-edge reveal path, a pinned toolbar while panels/focus are active, and simplified metrics copy.

- [ ] **Step 1: Add presentation imports and local toolbar state**

Import the pure functions and add module state:

```typescript
import {
  remoteSessionSummary,
  remoteToolbarHideDelay,
  remoteToolbarVisible,
  type RemoteToolbarVisibilityInput,
} from "./remote-session-presentation";

let remoteToolbarLastRevealedAtMs = 0;
let remoteToolbarPointerNearTop = false;
let remoteToolbarFocused = false;
let remoteToolbarTimer: number | null = null;
```

Register one document-level passive `pointermove` listener inside the existing `fullscreenListenerInitialized` guard. Do not register a listener during every render.

- [ ] **Step 2: Add direct DOM visibility functions**

Implement `remoteToolbarVisibilityInput(nowMs)`, `updateRemoteToolbarVisibility(nowMs)`, `revealRemoteToolbar()`, `scheduleRemoteToolbarHide()`, `handleRemoteToolbarPointerMove(event)`, and `resetRemoteToolbarState()`.

The input must set `panelOpen` from `transferPanelOpen` or a visible `[data-controller-text-panel]`, and `connected` from `snapshot?.runtime.state === "connected"`. `updateRemoteToolbarVisibility` must only change `session.dataset.remoteToolbarVisible`; it must never call `requestRender()`.

- [ ] **Step 3: Bind toolbar and panel pinning**

In `bindControllerInteractions`:

- on `.remote-toolbar` `pointerenter`, call `revealRemoteToolbar()` and set `remoteToolbarPointerNearTop = true`;
- on `pointerleave`, clear the flag and reschedule;
- on `focusin`, set `remoteToolbarFocused = true` and reveal;
- on `focusout`, defer one microtask, then set the flag from `toolbar.contains(document.activeElement)` and reschedule;
- after opening/closing transfer or text panels, call `revealRemoteToolbar()` or `updateRemoteToolbarVisibility()`;
- after `setupRemoteDesktop()`, call `updateRemoteToolbarVisibility()` so a DOM rebuild cannot leave stale visibility.

When native fullscreen becomes active, reset `remoteToolbarLastRevealedAtMs` and show the toolbar. When fullscreen exits, disconnect begins, or controller render cleanup runs, clear the timer and restore visible state.

- [ ] **Step 4: Replace technical toolbar counters with the user summary**

Use `remoteSessionSummary(config.width, config.height, videoQualityPreference, appliedVideoQuality)` in `renderRemoteDesktop()` and when a `videoQuality` signal arrives.

The `metrics` signal handler must continue assigning `relayCompletedFrames`, calling `updateVideoStartingMessage()` and `reportRenderMetrics()`, but must not write packet/frame counters into `[data-controller-metrics]`.

- [ ] **Step 5: Add the fullscreen guide and CSS visibility states**

Render this non-interactive guide inside `.remote-session`:

```html
<div class="remote-fullscreen-guide" aria-hidden="true">
  鼠标移到顶部显示工具栏 · Esc 退出全屏
</div>
```

Add CSS that shows the guide only in native fullscreen, transitions the toolbar and guide for 180 ms, and hides both when `data-remote-toolbar-visible="false"`. Hidden toolbar must have `pointer-events: none`; visible toolbar retains all current overlay positioning. Add a `prefers-reduced-motion: reduce` rule that disables these transitions.

- [ ] **Step 6: Verify TypeScript, behavior tests and production CSS**

Run from `apps/windows-ui`:

```powershell
bun test
bun run build
```

Expected: all tests pass, TypeScript emits no errors, and Vite produces production assets. Search built CSS/JS to confirm `data-remote-toolbar-visible`, the Chinese guide, and no removed technical toolbar copy.

- [ ] **Step 7: Review the task diff without committing**

Run `git diff --check`. Confirm fullscreen hide/show code does not call `requestRender`, send controller input, or change canvas geometry.

---

### Task 4: Version, Documentation and Windows Release Gates

**Files:**
- Modify: `apps/windows/Cargo.toml`
- Modify: `apps/windows-ui/src-tauri/Cargo.toml`
- Modify: `apps/windows-ui/package.json`
- Modify: `apps/windows-ui/src-tauri/tauri.conf.json`
- Modify: `tools/windows-installer/Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `README.md`
- Modify: `docs/windows-architecture-review.md`
- Create: `docs/windows-0.1.56-live-session-experience.md`

**Interfaces:**
- Consumes: completed Rust and TypeScript behavior from Tasks 1–3.
- Produces: version-aligned unsigned Windows 0.1.56 installer and validation manifests.

- [ ] **Step 1: Bump all Windows release versions to 0.1.56**

Change the five package/config versions and the three corresponding workspace package entries in `Cargo.lock` from `0.1.55` to `0.1.56`. Do not change third-party package versions.

- [ ] **Step 2: Document exact user-visible behavior and boundary**

Add `docs/windows-0.1.56-live-session-experience.md` covering automatic default, 30 FPS profiles, simplified toolbar, fullscreen reveal rules, unchanged protocol 8, no relay deployment, and unsigned installer status. Update README usage copy and the UX/performance bullets in the architecture review.

- [ ] **Step 3: Run complete automated gates**

Run:

```powershell
cargo fmt --all -- --check
python -X utf8 scripts/run-windows-cargo.py test --workspace --all-targets --all-features --jobs 1
python -X utf8 scripts/run-windows-cargo.py clippy --workspace --all-targets --all-features --jobs 1 -- -D warnings
python -X utf8 -m unittest discover -s scripts/tests -p "test_*.py" -v
python -X utf8 scripts/verify-managed-relay.py
```

Expected: all commands pass. The relay probe reports the existing managed endpoint; no deployment command is run.

- [ ] **Step 4: Build and verify the installer**

Run `python -X utf8 scripts/build-windows-installer.py`.

Expected: `dist/windows/DeskLinkSetup-0.1.56-x64.exe`, `windows-release-verification.json`, and `windows-installer-manifest.json` all report version 0.1.56 and `passed: true`; `signed` remains `false` until code-signing identity is configured.

- [ ] **Step 5: Final repository review without committing or pushing**

Run `git diff --check`, `git status -sb`, and inspect the installer SHA-256 from the manifest. Report changed files, validation results, artifact path, hash, unsigned status, and the remaining two-computer manual checks.
