import { describe, expect, test } from "bun:test";

import {
  formatRemainingTime,
  queuedFilesSummary,
  sampleTransferMetrics,
  transferMetricsLabel,
} from "./file-transfer-metrics";

describe("文件传输速度与剩余时间", () => {
  test("同一传输使用平滑速度并给出中文剩余时间", () => {
    const first = sampleTransferMetrics(null, {
      identity: "upload:report.zip:4194304",
      state: "sending",
      transferred: 0,
      total: 4_194_304,
    }, 1_000);
    const second = sampleTransferMetrics(first, {
      identity: "upload:report.zip:4194304",
      state: "sending",
      transferred: 1_048_576,
      total: 4_194_304,
    }, 2_000);

    expect(second.bytesPerSecond).toBe(1_048_576);
    expect(transferMetricsLabel(second, 1_048_576, 4_194_304)).toBe("1.0 MB/秒 · 预计 3 秒");
  });

  test("传输变化、进度回退或时钟回退时重新采样", () => {
    const previous = {
      identity: "upload:a.bin:1000",
      lastTransferred: 800,
      lastSampleAtMs: 2_000,
      bytesPerSecond: 400,
    };
    expect(sampleTransferMetrics(previous, {
      identity: "upload:a.bin:1000",
      state: "sending",
      transferred: 100,
      total: 1_000,
    }, 2_100).bytesPerSecond).toBeNull();
    expect(sampleTransferMetrics(previous, {
      identity: "download:a.bin:1000",
      state: "receiving",
      transferred: 900,
      total: 1_000,
    }, 2_100).bytesPerSecond).toBeNull();
    expect(sampleTransferMetrics(previous, {
      identity: "upload:a.bin:1000",
      state: "sending",
      transferred: 900,
      total: 1_000,
    }, 1_900).bytesPerSecond).toBeNull();
  });

  test("过密采样不会制造跳速，完成时不显示剩余时间", () => {
    const first = sampleTransferMetrics(null, {
      identity: "download:a.bin:1000",
      state: "receiving",
      transferred: 100,
      total: 1_000,
    }, 1_000);
    const early = sampleTransferMetrics(first, {
      identity: "download:a.bin:1000",
      state: "receiving",
      transferred: 200,
      total: 1_000,
    }, 1_100);
    expect(early).toBe(first);
    expect(transferMetricsLabel(first, 100, 1_000)).toBe("");
    expect(transferMetricsLabel({ ...first, bytesPerSecond: 500 }, 1_000, 1_000)).toBe("");
  });

  test("队列汇总忽略无效大小并限制时间文案", () => {
    expect(queuedFilesSummary([{ size: 1_048_576 }, { size: 524_288 }, { size: Number.NaN }]))
      .toBe("3 个文件，共 1.5 MB");
    expect(formatRemainingTime(0.2)).toBe("预计不到 1 秒");
    expect(formatRemainingTime(61)).toBe("预计 2 分钟");
    expect(formatRemainingTime(90_000)).toBe("预计超过 1 天");
  });
});
