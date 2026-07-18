import { describe, expect, test } from "bun:test";

import {
  markTransferResultsRead,
  recordTransferResult,
  transferProgressPaintDelay,
  type TransferActivityState,
} from "./file-transfer-activity";

const emptyActivity: TransferActivityState = { unreadResults: 0, tone: null };

describe("文件传输界面刷新与未读状态", () => {
  test("连续进度最多每 100 毫秒刷新一次", () => {
    expect(transferProgressPaintDelay("sending", "sending", 1_000, 1_030)).toBe(70);
    expect(transferProgressPaintDelay("receiving", "receiving", 1_000, 1_100)).toBe(0);
  });

  test("状态变化、终态和异常时钟立即刷新", () => {
    expect(transferProgressPaintDelay("waiting", "sending", 1_000, 1_010)).toBe(0);
    expect(transferProgressPaintDelay("sending", "completed", 1_000, 1_010)).toBe(0);
    expect(transferProgressPaintDelay("receiving", "receiving", 1_000, 900)).toBe(0);
    expect(transferProgressPaintDelay(null, "receiving", null, 1_000)).toBe(0);
  });

  test("面板关闭时累积终态，重复信号不会重复计数", () => {
    const completed = { state: "completed" as const, direction: "download" as const, name: "报告.pdf", total: 10 };
    const first = recordTransferResult(emptyActivity, null, completed, false);
    expect(first).toEqual({ unreadResults: 1, tone: "success" });
    expect(recordTransferResult(first, completed, completed, false)).toBe(first);
    const failed = recordTransferResult(first, completed, {
      state: "failed",
      direction: "upload",
      name: "数据.zip",
      total: 20,
    }, false);
    expect(failed).toEqual({ unreadResults: 2, tone: "error" });
  });

  test("面板打开时结果视为已读，主动打开也能清除计数", () => {
    const result = recordTransferResult({ unreadResults: 3, tone: "error" }, null, {
      state: "completed",
      direction: "upload",
      name: "完成.txt",
      total: 1,
    }, true);
    expect(result).toEqual({ unreadResults: 0, tone: "success" });
    expect(markTransferResultsRead({ unreadResults: 2, tone: "error" }))
      .toEqual({ unreadResults: 0, tone: "error" });
  });
});
