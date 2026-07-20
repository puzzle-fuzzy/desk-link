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

  test("写盘失败保留准确原因并防止重复重试", () => {
    expect(fileQueueProtectionPresentation("memoryOnly", "  磁盘暂时不可用。  ", true)).toEqual({
      tone: "warning",
      message: "磁盘暂时不可用。",
      retryLabel: "正在重试…",
      retryDisabled: true,
    });
  });
});
