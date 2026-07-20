import { describe, expect, test } from "bun:test";

import {
  fileRecoveryAvailabilityAfterSignal,
  preferredDeviceIdForRecovery,
  recoveredFileTransfer,
} from "./file-transfer-recovery";

describe("文件传输跨重启恢复", () => {
  test("恢复发送任务时只向界面提供安全摘要", () => {
    expect(recoveredFileTransfer({
      revision: 1,
      deviceId: "123 456 789 012",
      direction: "upload",
      name: "报告.pdf",
      total: 4096,
      message: "上次文件发送未完成；连接设备后可以从断点重试。",
    })).toEqual({
      kind: "fileTransfer",
      state: "failed",
      direction: "upload",
      name: "报告.pdf",
      transferred: 0,
      total: 4096,
      message: "上次文件发送未完成；连接设备后可以从断点重试。 目标设备：123 456 789 012。",
    });
  });

  test("恢复接收任务时保持重新获取方向", () => {
    const restored = recoveredFileTransfer({
      revision: 1,
      deviceId: "123 456 789 012",
      direction: "download",
      name: "远端文件",
      total: 0,
      message: "上次远端文件获取未完成；连接设备后可以重新获取。",
    });

    expect(restored.direction).toBe("download");
    expect(restored.state).toBe("failed");
  });

  test("只有真实开始的任务才产生恢复入口", () => {
    expect(fileRecoveryAvailabilityAfterSignal(false, "failed")).toBeFalse();
    expect(fileRecoveryAvailabilityAfterSignal(false, "waiting")).toBeTrue();
    expect(fileRecoveryAvailabilityAfterSignal(true, "cancelled")).toBeTrue();
    expect(fileRecoveryAvailabilityAfterSignal(true, "completed")).toBeFalse();
  });

  test("恢复任务优先填入它所属的设备", () => {
    expect(preferredDeviceIdForRecovery("", "123 456 789 012", "987 654 321 098"))
      .toBe("123 456 789 012");
    expect(preferredDeviceIdForRecovery("111 222 333 444", "123 456 789 012", null))
      .toBe("111 222 333 444");
    expect(preferredDeviceIdForRecovery("", null, "987 654 321 098"))
      .toBe("987 654 321 098");
  });
});
