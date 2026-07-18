import { describe, expect, test } from "bun:test";

import {
  connectionProgressPresentation,
  formatConnectionElapsed,
} from "./connection-progress";

describe("controller connection progress", () => {
  test("maps runtime states to the four user-visible stages", () => {
    expect(connectionProgressPresentation("finding", 0).activeStep).toBe(0);
    expect(connectionProgressPresentation("connecting", 0).activeStep).toBe(1);
    expect(connectionProgressPresentation("waitingApproval", 0).activeStep).toBe(2);
    expect(connectionProgressPresentation("connected", 0).activeStep).toBe(3);
    expect(connectionProgressPresentation("idle", 0).activeStep).toBe(-1);
  });

  test("adds actionable recovery copy after a long wait", () => {
    const early = connectionProgressPresentation("waitingApproval", 14);
    const delayed = connectionProgressPresentation("waitingApproval", 15);
    expect(early.delayed).toBe(false);
    expect(delayed.delayed).toBe(true);
    expect(delayed.guidance).toContain("取消后重新连接");
  });

  test("formats stable Chinese elapsed time", () => {
    expect(formatConnectionElapsed(-1)).toBe("刚刚开始");
    expect(formatConnectionElapsed(9.9)).toBe("已等待 9 秒");
    expect(formatConnectionElapsed(60)).toBe("已等待 1 分钟");
    expect(formatConnectionElapsed(125)).toBe("已等待 2 分 5 秒");
  });
});
