export interface VideoRenderTimingSnapshot {
  displayedFpsX100: number | null;
  maxFrameGapMs: number | null;
  coalescedFrameDrops: number;
}

const MAX_FRAME_GAP_MS = 60_000;
const MAX_FPS_X100 = 12_000;
const QUALITY_WINDOW_MS = 10_000;

/**
 * Low-overhead timing for the canvas presentation path.
 *
 * `observe` mutates counters in place so the hot frame callback does not
 * allocate. A snapshot is created only when diagnostics are reported.
 */
export class VideoRenderTiming {
  private displayedFrames = 0;
  private firstPresentedAtMs: number | null = null;
  private previousPresentedAtMs: number | null = null;
  private maxGapMs = 0;
  private coalescedFrameDrops = 0;
  private pendingCoalescedFrameDrops = 0;
  private recentWindowStartedAtMs: number | null = null;
  private recentPreviousPresentedAtMs: number | null = null;
  private recentDisplayedFrames = 0;
  private recentMaxGapMs = 0;

  reset(): void {
    this.displayedFrames = 0;
    this.firstPresentedAtMs = null;
    this.previousPresentedAtMs = null;
    this.maxGapMs = 0;
    this.coalescedFrameDrops = 0;
    this.pendingCoalescedFrameDrops = 0;
    this.recentWindowStartedAtMs = null;
    this.recentPreviousPresentedAtMs = null;
    this.recentDisplayedFrames = 0;
    this.recentMaxGapMs = 0;
  }

  recordCoalescedFrame(): void {
    this.coalescedFrameDrops += 1;
    this.pendingCoalescedFrameDrops += 1;
  }

  takeCoalescedFrameDrops(): number {
    const drops = this.pendingCoalescedFrameDrops;
    this.pendingCoalescedFrameDrops = 0;
    return drops;
  }

  observe(presentedAtMs: number): void {
    if (!Number.isFinite(presentedAtMs) || presentedAtMs < 0) {
      return;
    }
    if (this.firstPresentedAtMs === null) {
      this.firstPresentedAtMs = presentedAtMs;
    }
    if (this.previousPresentedAtMs !== null) {
      const gap = Math.min(
        MAX_FRAME_GAP_MS,
        Math.max(0, Math.round(presentedAtMs - this.previousPresentedAtMs)),
      );
      this.maxGapMs = Math.max(this.maxGapMs, gap);
    }
    this.previousPresentedAtMs = presentedAtMs;
    this.displayedFrames += 1;

    if (
      this.recentWindowStartedAtMs === null
      || presentedAtMs - this.recentWindowStartedAtMs >= QUALITY_WINDOW_MS
    ) {
      this.recentWindowStartedAtMs = presentedAtMs;
      this.recentPreviousPresentedAtMs = null;
      this.recentDisplayedFrames = 0;
      this.recentMaxGapMs = 0;
    }
    if (this.recentPreviousPresentedAtMs !== null) {
      const recentGap = Math.min(
        MAX_FRAME_GAP_MS,
        Math.max(0, Math.round(presentedAtMs - this.recentPreviousPresentedAtMs)),
      );
      this.recentMaxGapMs = Math.max(this.recentMaxGapMs, recentGap);
    }
    this.recentPreviousPresentedAtMs = presentedAtMs;
    this.recentDisplayedFrames += 1;
  }

  snapshot(nowMs: number): VideoRenderTimingSnapshot {
    if (
      this.displayedFrames < 2
      || this.firstPresentedAtMs === null
      || !Number.isFinite(nowMs)
      || nowMs <= this.firstPresentedAtMs
    ) {
      return {
        displayedFpsX100: null,
        maxFrameGapMs: this.displayedFrames > 1 ? this.maxGapMs : null,
        coalescedFrameDrops: this.coalescedFrameDrops,
      };
    }
    const elapsedMs = Math.max(1, nowMs - this.firstPresentedAtMs);
    const displayedFpsX100 = Math.min(
      MAX_FPS_X100,
      Math.max(0, Math.round((this.displayedFrames * 100_000) / elapsedMs)),
    );
    return {
      displayedFpsX100,
      maxFrameGapMs: this.maxGapMs,
      coalescedFrameDrops: this.coalescedFrameDrops,
    };
  }

  /**
   * Returns a short rolling-window view for user-facing quality feedback.
   * The cumulative snapshot above remains the source of truth for diagnostics.
   */
  recentSnapshot(nowMs: number): VideoRenderTimingSnapshot {
    if (
      this.recentDisplayedFrames < 2
      || this.recentWindowStartedAtMs === null
      || !Number.isFinite(nowMs)
      || nowMs <= this.recentWindowStartedAtMs
    ) {
      return {
        displayedFpsX100: null,
        maxFrameGapMs: this.recentDisplayedFrames > 1 ? this.recentMaxGapMs : null,
        coalescedFrameDrops: this.coalescedFrameDrops,
      };
    }
    const elapsedMs = Math.max(
      1,
      Math.min(QUALITY_WINDOW_MS, nowMs - this.recentWindowStartedAtMs),
    );
    const staleGap = this.recentPreviousPresentedAtMs === null
      ? 0
      : Math.min(
        MAX_FRAME_GAP_MS,
        Math.max(0, Math.round(nowMs - this.recentPreviousPresentedAtMs)),
      );
    return {
      displayedFpsX100: Math.min(
        MAX_FPS_X100,
        Math.max(0, Math.round((this.recentDisplayedFrames * 100_000) / elapsedMs)),
      ),
      maxFrameGapMs: Math.max(this.recentMaxGapMs, staleGap),
      coalescedFrameDrops: this.coalescedFrameDrops,
    };
  }
}
