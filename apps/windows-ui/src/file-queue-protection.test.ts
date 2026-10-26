import { describe, expect, test } from "bun:test";
import { fileQueueProtectionPresentation } from "./file-queue-protection";

describe("等待队列加密保护状态", () => {
  test("空队列不显示无意义的保护状态", () => {
    expect(fileQueueProtectionPresentation("empty", null, false)).toBeNull();
  });

  test("成功保存时明确说明由当前 Windows 账户保护", () => {
    expect(fileQueueProtectionPresentation("protected", null, false)).toEqual({
      tone: "protected",
      message: "等待队列已由当前 Windows 账户加密保存",
      retryLabel: null,
      retryDisabled: false,
    });
  });

  test("入队保存中不宣称已经持久化，也不显示失败重试入口", () => {
    expect(fileQueueProtectionPresentation("protecting", null, false)).toEqual({
      tone: "protecting",
      message: "正在加密保存等待队列；保存完成后可在重启后恢复。",
      retryLabel: null,
      retryDisabled: true,
    });
    expect(fileQueueProtectionPresentation("protecting", "正在更新已保存的队列。", true)?.retryLabel).toBeNull();
    expect(fileQueueProtectionPresentation("protecting", "正在更新已保存的队列。", true)?.message).toBe("正在更新已保存的队列。");
  });

  test("写盘失败保留准确原因并防止重复重试", () => {
    expect(fileQueueProtectionPresentation("memoryOnly", "  磁盘暂时不可用。  ", true)).toEqual({
      tone: "warning",
      message: "磁盘暂时不可用。",
      retryLabel: "正在重试…",
      retryDisabled: true,
    });
  });
});
