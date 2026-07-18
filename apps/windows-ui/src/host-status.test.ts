import { describe, expect, test } from "bun:test";

import { hostStatusSummary } from "./host-status";
import type { HostRuntimeState, HostSnapshot } from "./types";

const configuredConnection = {
  relayAddress: "relay.example.com:4433",
  serverName: "relay.example.com",
  sessionId: "session",
  streamId: 1,
  hasSavedKey: true,
};

function hostSnapshot(
  runtimeState: HostRuntimeState,
  overrides: Partial<HostSnapshot> = {},
): HostSnapshot {
  return {
    readiness: "configured",
    title: "主机已准备",
    detail: "主机运行中",
    runtime: {
      state: runtimeState,
      title: "主机运行状态",
      detail: "主机运行详情",
      tooltip: "DeskLink 主机",
    },
    connection: configuredConnection,
    connectionError: null,
    trustedControllers: [],
    trustedError: null,
    relayStatus: {
      mode: "external",
      state: "ready",
      title: "中继已配置",
      detail: "可以连接",
    },
    diagnosticChecks: [],
    pairingActive: false,
    pendingApproval: null,
    fixedPasswordEnabled: false,
    fixedPasswordError: null,
    deviceId: "123 456 789 012",
    refreshedAtUnixS: 1,
    ...overrides,
  };
}

describe("Windows host status summary", () => {
  test("gives protected-data errors first priority", () => {
    for (const error of ["connectionError", "trustedError", "fixedPasswordError"] as const) {
      const summary = hostStatusSummary(hostSnapshot("available", { [error]: "无法读取" }));
      expect(summary.title).toBe("需要处理");
    }
  });

  test("surfaces a pending approval before runtime progress", () => {
    const summary = hostStatusSummary(hostSnapshot("connecting", {
      pendingApproval: {
        requestId: 3,
        deviceId: "abcd",
        fingerprint: "fingerprint",
        expiresAtUnixS: 10,
      },
    }));

    expect(summary.title).toBe("等待确认");
  });

  test("does not call a stopped persisted connection available", () => {
    expect(hostStatusSummary(hostSnapshot("stopped")).title).toBe("未开启共享");
    expect(hostStatusSummary(hostSnapshot("notConfigured", { connection: null })).title).toBe("未开启共享");
    expect(hostStatusSummary(hostSnapshot("starting", { connection: null })).title).toBe("未开启共享");
  });

  test("maps transitional runtime states to connecting", () => {
    for (const state of ["starting", "pairing", "connecting", "reconnecting"] as const) {
      expect(hostStatusSummary(hostSnapshot(state)).title).toBe("连接中");
    }
  });

  test("distinguishes availability from an active sharing session", () => {
    expect(hostStatusSummary(hostSnapshot("available")).title).toBe("本机可被连接");

    const active = hostStatusSummary(hostSnapshot("connected"));
    expect(active.title).toBe("正在共享本机");
    expect(active.detail).toBe("远程设备正在查看并控制这台电脑");
  });
});
