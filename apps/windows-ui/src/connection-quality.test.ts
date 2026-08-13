import { describe, expect, test } from "bun:test";

import { connectionQualitySummary } from "./connection-quality";

describe("远程连接质量", () => {
  test("首帧阶段先显示测量中，不提前宣称稳定", () => {
    expect(connectionQualitySummary({
      decodedFrames: 3,
      displayedFpsX100: null,
      maxFrameGapMs: null,
      receivedVideoPackets: 0,
      droppedVideoPackets: 0,
    })).toEqual({
      tone: "measuring",
      label: "正在测量",
      detail: "正在收集画面帧率和传输稳定性。",
    });
  });

  test("稳定的 24 FPS 画面显示流畅", () => {
    expect(connectionQualitySummary({
      decodedFrames: 120,
      displayedFpsX100: 2_400,
      maxFrameGapMs: 48,
      receivedVideoPackets: 1_000,
      droppedVideoPackets: 5,
    }).tone).toBe("good");
  });

  test("中等帧率和少量丢包显示基本稳定", () => {
    expect(connectionQualitySummary({
      decodedFrames: 80,
      displayedFpsX100: 1_800,
      maxFrameGapMs: 220,
      receivedVideoPackets: 920,
      droppedVideoPackets: 60,
    }).tone).toBe("attention");
  });

  test("严重丢帧或低帧率给出可行动的关注提示", () => {
    const summary = connectionQualitySummary({
      decodedFrames: 80,
      displayedFpsX100: 900,
      maxFrameGapMs: 600,
      receivedVideoPackets: 700,
      droppedVideoPackets: 180,
    });
    expect(summary.tone).toBe("poor");
    expect(summary.detail).toContain("流畅");
  });
});
