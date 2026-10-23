import { describe, expect, test } from "bun:test";

import { hostSnapshotPresentationChanged } from "./host-snapshot-presentation";
import type { HostApprovalSummary, HostSnapshot } from "./types";

function hostSnapshot(overrides: Partial<HostSnapshot> = {}): HostSnapshot {
  return {
    readiness: "configured",
    title: "主机已准备",
    detail: "主机运行中",
    runtime: { state: "available", title: "可被连接", detail: "等待连接", tooltip: "主机在线" },
    connection: {
      relayAddress: "relay.example.com:4433",
      serverName: "relay.example.com",
      sessionId: "session",
      streamId: 1,
      hasSavedKey: true,
    },
    connectionError: null,
    trustedControllers: [],
    trustedError: null,
    relayStatus: { mode: "external", state: "ready", title: "已配置", detail: "中继已连接" },
    diagnosticChecks: [],
    pairingActive: false,
    pendingApproval: null,
    fixedPasswordEnabled: false,
    fixedPasswordError: null,
    deviceId: "123456789012",
    refreshedAtUnixS: 1,
    ...overrides,
  };
}

const approval: HostApprovalSummary = {
  requestId: 3,
  deviceId: "controller",
  fingerprint: "fingerprint",
  expiresAtUnixS: 100,
  identityChanged: false,
};

describe("host snapshot presentation updates", () => {
  test("shows the first host state and removes unavailable state", () => {
    expect(hostSnapshotPresentationChanged(null, hostSnapshot())).toBe(true);
    expect(hostSnapshotPresentationChanged(hostSnapshot(), null)).toBe(true);
    expect(hostSnapshotPresentationChanged(null, null)).toBe(false);
  });

  test("shows a new approval on the controller page and removes a resolved request", () => {
    const idle = hostSnapshot();
    const pending = hostSnapshot({ pendingApproval: approval });
    expect(hostSnapshotPresentationChanged(idle, pending)).toBe(true);
    expect(hostSnapshotPresentationChanged(pending, idle)).toBe(true);
  });

  test("refreshes request identity and expiry even when its request ID stays the same", () => {
    const previous = hostSnapshot({ pendingApproval: approval });
    for (const changed of [
      { requestId: 4 },
      { fingerprint: "new-fingerprint", identityChanged: true },
      { deviceId: "another-controller" },
      { expiresAtUnixS: 200 },
    ]) {
      expect(hostSnapshotPresentationChanged(previous, hostSnapshot({
        pendingApproval: { ...approval, ...changed },
      }))).toBe(true);
    }
  });

  test("refreshes sharing state, protected-data warnings and device summaries", () => {
    const previous = hostSnapshot();
    const changes: Partial<HostSnapshot>[] = [
      { readiness: "attention" },
      { runtime: { ...previous.runtime, state: "connected" } },
      { connectionError: "连接设置无法读取" },
      { trustedError: "已批准设备无法读取" },
      { fixedPasswordError: "固定密码无法读取" },
      { connection: null },
      { connection: { ...previous.connection!, relayAddress: "new-relay.example.com:4433" } },
      { deviceId: "999999999999" },
      { pairingActive: true },
      { fixedPasswordEnabled: true },
      { trustedControllers: [{ deviceId: "controller", verifyKey: "key", fingerprint: "print", approvedAtUnixS: 1 }] },
    ];
    for (const change of changes) {
      expect(hostSnapshotPresentationChanged(previous, hostSnapshot(change))).toBe(true);
    }
  });

  test("keeps the remote canvas intact for timestamp-only or equal snapshots", () => {
    const previous = hostSnapshot({ pendingApproval: approval });
    expect(hostSnapshotPresentationChanged(previous, {
      ...structuredClone(previous),
      refreshedAtUnixS: 200,
    })).toBe(false);
    expect(hostSnapshotPresentationChanged(previous, structuredClone(previous))).toBe(false);
  });
});
