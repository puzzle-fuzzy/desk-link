import { describe, expect, test } from "bun:test";

import {
  MAX_REPORTED_DECODE_QUEUE_SIZE,
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
    expect(pressure.takeSample()).toEqual({
      peakDecodeQueueSize: 7,
      freshnessRecoveries: 1,
    });
    expect(pressure.takeSample()).toEqual({
      peakDecodeQueueSize: 0,
      freshnessRecoveries: 0,
    });
  });

  test("无效队列值被规范化且上报值保持有界", () => {
    const pressure = new VideoPlaybackPressure();

    pressure.observe(Number.NaN, 1_000);
    pressure.observe(-10, 1_010);
    pressure.observe(1_000, 1_020);
    expect(pressure.takeSample()).toEqual({
      peakDecodeQueueSize: MAX_REPORTED_DECODE_QUEUE_SIZE,
      freshnessRecoveries: 0,
    });
  });

  test("切换视频流会清除样本和恢复冷却", () => {
    const pressure = new VideoPlaybackPressure();

    pressure.observe(5, 1_000);
    pressure.observe(5, 1_010);
    expect(pressure.observe(5, 1_020)).toBe("recover");
    pressure.reset();

    expect(pressure.takeSample()).toEqual({
      peakDecodeQueueSize: 0,
      freshnessRecoveries: 0,
    });
    pressure.observe(5, 1_030);
    pressure.observe(5, 1_040);
    expect(pressure.observe(5, 1_050)).toBe("recover");
  });
});
