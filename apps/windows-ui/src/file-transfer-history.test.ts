import { describe, expect, test } from "bun:test";
import { appendTransferHistory, type TransferHistoryEntry } from "./file-transfer-history";

const sending = {
  state: "sending" as const,
  direction: "upload" as const,
  name: "报告.pdf",
  total: 1024,
  message: "正在发送",
};

describe("文件传输记录", () => {
  test("只记录完成、失败、拒绝和取消状态", () => {
    expect(appendTransferHistory([], null, sending, 1, 100)).toEqual([]);
    const completed = { ...sending, state: "completed" as const, message: "发送完成" };
    expect(appendTransferHistory([], sending, completed, 1, 100)).toEqual([{
      id: 1,
      direction: "upload",
      name: "报告.pdf",
      size: 1024,
      state: "completed",
      finishedAtMs: 100,
    }]);
  });

  test("忽略重复终态，但保留同名文件的下一次任务", () => {
    const completed = { ...sending, state: "completed" as const, message: "发送完成" };
    const once = appendTransferHistory([], sending, completed, 1, 100);
    expect(appendTransferHistory(once, completed, completed, 2, 200)).toBe(once);
    expect(appendTransferHistory(once, sending, completed, 2, 200)).toHaveLength(2);
  });

  test("记录始终限制为最近八项", () => {
    const failed = { ...sending, state: "failed" as const, message: "发送失败" };
    let history: TransferHistoryEntry[] = [];
    for (let id = 1; id <= 10; id += 1) {
      history = appendTransferHistory(history, sending, failed, id, id);
    }
    expect(history.map((entry) => entry.id)).toEqual([10, 9, 8, 7, 6, 5, 4, 3]);
  });
});
