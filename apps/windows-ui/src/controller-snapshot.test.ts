import { describe, expect, test } from "bun:test";
import { mergeControllerOperationSnapshot } from "./controller-snapshot";
import type { ControllerSnapshot } from "./types";

function snapshot(state: ControllerSnapshot["runtime"]["state"], deviceId: string): ControllerSnapshot {
  return {
    runtime: {
      state,
      title: state,
      detail: `${state} detail`,
      streamId: state === "connected" ? 9 : null,
    },
    savedConnection: null,
    connectionError: null,
    savedDevices: [{
      deviceId,
      alias: null,
      persistent: true,
      lastUsedUnixS: 1,
    }],
    savedDevicesError: null,
    fileRecovery: null,
    fileRecoveryError: null,
    fileQueueRecovery: null,
    fileQueueRecoveryError: null,
  };
}

describe("controller operation snapshot merge", () => {
  test("uses the invoke result when no newer status signal arrived", () => {
    const result = mergeControllerOperationSnapshot(
      snapshot("connected", "new-device"),
      snapshot("finding", "old-device"),
      4,
      4,
    );

    expect(result.runtime.state).toBe("connected");
    expect(result.savedDevices[0]?.deviceId).toBe("new-device");
  });

  test("keeps the live runtime while refreshing the rest of the snapshot", () => {
    const result = mergeControllerOperationSnapshot(
      snapshot("finding", "new-device"),
      snapshot("connected", "old-device"),
      4,
      5,
    );

    expect(result.runtime.state).toBe("connected");
    expect(result.runtime.streamId).toBe(9);
    expect(result.savedDevices[0]?.deviceId).toBe("new-device");
  });

  test("does not invent a runtime when the live snapshot is absent", () => {
    const result = mergeControllerOperationSnapshot(
      snapshot("connected", "new-device"),
      null,
      4,
      5,
    );

    expect(result.runtime.state).toBe("connected");
  });
});
