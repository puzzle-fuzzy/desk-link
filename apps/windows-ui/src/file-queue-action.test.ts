import { describe, expect, test } from "bun:test";
import { FileQueueActionGate } from "./file-queue-action";

describe("等待队列操作互斥", () => {
  test("后端确认返回前拒绝第二个操作", () => {
    const gate = new FileQueueActionGate();
    const first = gate.begin("clear");

    expect(first).not.toBeNull();
    expect(gate.busy).toBe(true);
    expect(gate.begin("resume")).toBeNull();
    expect(gate.matches("clear")).toBe(true);

    expect(gate.finish(first!)).toBe(true);
    expect(gate.busy).toBe(false);
  });

  test("移除操作只标记对应文件", () => {
    const gate = new FileQueueActionGate();
    const action = gate.begin("remove", "0102");

    expect(gate.matches("remove", "0102")).toBe(true);
    expect(gate.matches("remove", "0304")).toBe(false);
    gate.finish(action!);
  });

  test("迟到的完成通知不能清除更新的操作", () => {
    const gate = new FileQueueActionGate();
    const oldAction = gate.begin("protect")!;
    gate.finish(oldAction);
    const currentAction = gate.begin("resume")!;

    expect(gate.finish(oldAction)).toBe(false);
    expect(gate.matches("resume")).toBe(true);
    expect(gate.finish(currentAction)).toBe(true);
  });
});
