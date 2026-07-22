import { describe, expect, test } from "bun:test";

import { VideoRenderTiming } from "./video-render-timing";

describe("video render timing", () => {
  test("does not report an fps before two displayed frames", () => {
    const timing = new VideoRenderTiming();
    timing.observe(100);
    expect(timing.snapshot(1_000)).toEqual({
      displayedFpsX100: null,
      maxFrameGapMs: null,
      coalescedFrameDrops: 0,
    });
  });

  test("reports a bounded average fps and the worst frame gap", () => {
    const timing = new VideoRenderTiming();
    timing.observe(100);
    timing.recordCoalescedFrame();
    timing.observe(133);
    timing.observe(300);
    expect(timing.snapshot(1_100)).toEqual({
      displayedFpsX100: 300,
      maxFrameGapMs: 167,
      coalescedFrameDrops: 1,
    });
  });

  test("takes display-drop samples without losing the cumulative diagnostic count", () => {
    const timing = new VideoRenderTiming();
    timing.recordCoalescedFrame();
    timing.recordCoalescedFrame();
    expect(timing.takeCoalescedFrameDrops()).toBe(2);
    expect(timing.takeCoalescedFrameDrops()).toBe(0);
    expect(timing.snapshot(100)).toEqual({
      displayedFpsX100: null,
      maxFrameGapMs: null,
      coalescedFrameDrops: 2,
    });
  });

  test("ignores invalid timestamps and resets between streams", () => {
    const timing = new VideoRenderTiming();
    timing.observe(Number.NaN);
    timing.observe(10);
    timing.observe(20);
    expect(timing.snapshot(30).maxFrameGapMs).toBe(10);
    timing.reset();
    expect(timing.snapshot(30)).toEqual({
      displayedFpsX100: null,
      maxFrameGapMs: null,
      coalescedFrameDrops: 0,
    });
  });

  test("caps pathological gaps and rates", () => {
    const timing = new VideoRenderTiming();
    timing.observe(0);
    timing.observe(1_000_000);
    expect(timing.snapshot(1_000_001)).toEqual({
      displayedFpsX100: 0,
      maxFrameGapMs: 60_000,
      coalescedFrameDrops: 0,
    });
  });
});
