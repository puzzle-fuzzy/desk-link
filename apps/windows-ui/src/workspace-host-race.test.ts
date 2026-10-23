import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { canRecoverWorkspaceSnapshot } from "./account-mode";
import { hostSnapshotPresentationChanged } from "./host-snapshot-presentation";
import { LatestRequest } from "./latest-request";
import type { HostSnapshot } from "./types";

const source = readFileSync(resolve(import.meta.dir, "main.ts"), "utf8");
function operationSource(startMarker: string, endMarker: string): string {
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker, start);
  if (start < 0 || end < 0) throw new Error("Unable to locate workspace host operation");
  return new Bun.Transpiler({ loader: "ts" }).transformSync(source.slice(start, end));
}
const startup = operationSource("function ensureWorkspaceHostListeners()", "\nvoid bootstrap();");
const refresh = operationSource("async function refreshSnapshot(", "\nfunction scheduleSnapshotRecovery()");

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function flushPromises(): Promise<void> {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
}

function startupHost(listen: (eventName: string, changed: () => void) => Promise<unknown>) {
  const actions: string[] = [];
  const timers = new Map<number, () => void>();
  const events = new Map<string, () => void>();
  const intervals: Array<() => void> = [];
  const document = { visibilityState: "visible", addEventListener: (name: string, callback: () => void) => events.set(name, callback) };
  let nextTimer = 0;
  const environment = {
    listen, document,
    window: {
      addEventListener: (name: string, callback: () => void) => events.set(name, callback),
      setTimeout: (callback: () => void) => { timers.set(++nextTimer, callback); return nextTimer; },
      clearTimeout: (timer: number) => timers.delete(timer),
      setInterval: (callback: () => void) => intervals.push(callback),
    },
    workspaceHostEvents: ["host-runtime-changed", "host-approval-changed"],
    workspaceHostListeners: new Set<string>(),
    workspaceHostListenerAttempts: new Map<string, Promise<void>>(),
    workspaceRecoveryBound: false,
    account: { signedIn: false }, accountLocalMode: true,
    canRecoverWorkspaceSnapshot,
    getVersion: () => Promise.resolve("test"), applicationVersion: "", activeView: "controller",
    checkForWindowsUpdate: () => Promise.resolve(), render: () => {}, scheduleRender: () => {},
    initializeController: async () => { actions.push("initialize-controller"); },
    refreshSnapshot: async (showLoading = true) => { actions.push(showLoading ? "initial-snapshot" : "background-snapshot"); },
  };
  const create = new Function(...Object.keys(environment), `${startup}
    return { startWorkspace, ensureWorkspaceHostListeners, leaveWorkspace() { accountLocalMode = false; } };
  `);
  const operations = create(...Object.values(environment)) as {
    startWorkspace(): Promise<void>;
    ensureWorkspaceHostListeners(): Promise<void>;
    leaveWorkspace(): void;
  };
  return { ...operations, actions, timers, events, intervals, document };
}

function snapshot(state: "connecting" | "available", refreshedAtUnixS = 1): HostSnapshot {
  return {
    readiness: "configured", title: "host", detail: "host",
    runtime: { state, title: state, detail: state, tooltip: state },
    connection: { relayAddress: "relay.example:4433", serverName: "relay.example", sessionId: "test", streamId: 1, hasSavedKey: true },
    connectionError: null, trustedControllers: [], trustedError: null,
    relayStatus: { mode: "external", state: "ready", title: "relay", detail: "relay" },
    diagnosticChecks: [], pairingActive: false, pendingApproval: null,
    fixedPasswordEnabled: false, fixedPasswordError: null, deviceId: "123456789012", refreshedAtUnixS,
  };
}

function snapshotHost(requests: Array<Promise<HostSnapshot>>, initial: HostSnapshot | null = null) {
  let renders = 0;
  const environment = {
    snapshotRequests: new LatestRequest(), snapshot: initial,
    getHostSnapshot: () => requests.shift()!, hostSnapshotPresentationChanged,
    loading: false, feedback: null, snapshotReadFeedback: null,
    snapshotRecoveryTimer: null, pairingSession: null, activeView: "connection",
    window: { clearTimeout: () => {} },
    clearPairingSecrets: () => {}, clearFixedAccessSecrets: () => {}, scheduleSnapshotRecovery: () => {},
    normalizeError: (error: unknown) => (error as Error).message,
    render: () => { renders += 1; },
  };
  const create = new Function(...Object.keys(environment), `${refresh}
    return { refreshSnapshot, state: () => ({ snapshot, loading, feedback }) };
  `);
  const operations = create(...Object.values(environment)) as {
    refreshSnapshot(showLoading?: boolean): Promise<void>;
    state(): { snapshot: HostSnapshot | null; loading: boolean; feedback: unknown };
  };
  return { ...operations, renders: () => renders };
}

describe("workspace host startup and recovery", () => {
  test("arms both host listeners before requesting the initial snapshot", async () => {
    const runtime = deferred<void>();
    const approval = deferred<void>();
    const subscribed: string[] = [];
    const host = startupHost(async (name) => {
      subscribed.push(name);
      await (name === "host-runtime-changed" ? runtime.promise : approval.promise);
    });
    const started = host.startWorkspace();
    await flushPromises();
    expect(subscribed).toEqual(["host-runtime-changed", "host-approval-changed"]);
    expect(host.actions).toEqual([]);
    runtime.resolve();
    await flushPromises();
    expect(host.actions).toEqual([]);
    approval.resolve();
    await started;
    expect(host.actions).toEqual(["initialize-controller", "initial-snapshot"]);
    expect(host.timers.size).toBe(0);
  });

  test("recovers rejected subscriptions without duplicating successful listeners", async () => {
    const counts = new Map<string, number>();
    const host = startupHost(async (name) => {
      const count = (counts.get(name) ?? 0) + 1;
      counts.set(name, count);
      if (name === "host-runtime-changed" && count === 1) throw new Error("temporary subscription failure");
    });
    await host.startWorkspace();
    expect(host.actions).toContain("initial-snapshot");
    await host.ensureWorkspaceHostListeners();
    expect(counts.get("host-runtime-changed")).toBe(2);
    expect(counts.get("host-approval-changed")).toBe(1);
  });

  test("a stalled subscription cannot leave startup loading forever", async () => {
    const never = deferred<void>();
    const host = startupHost(() => never.promise);
    const started = host.startWorkspace();
    await flushPromises();
    expect(host.actions).toEqual([]);
    [...host.timers.values()][0]!();
    await started;
    expect(host.actions).toEqual(["initialize-controller", "initial-snapshot"]);
    never.resolve();
    await flushPromises();
  });

  test("visible-window recovery is bound once and stays quiet while hidden or signed out", async () => {
    const host = startupHost(async () => {});
    await host.startWorkspace();
    await host.startWorkspace();
    expect(host.intervals).toHaveLength(1);
    host.actions.length = 0;
    host.document.visibilityState = "hidden";
    host.intervals[0]!();
    expect(host.actions).toEqual([]);
    host.document.visibilityState = "visible";
    host.events.get("focus")!();
    expect(host.actions).toEqual(["background-snapshot"]);
    host.leaveWorkspace();
    host.intervals[0]!();
    expect(host.actions).toEqual(["background-snapshot"]);
  });
});

describe("host snapshot request ordering", () => {
  test("an Available event wins over a late initial Connecting snapshot", async () => {
    const initial = deferred<HostSnapshot>();
    const event = deferred<HostSnapshot>();
    const host = snapshotHost([initial.promise, event.promise]);
    const first = host.refreshSnapshot();
    const second = host.refreshSnapshot(false);
    event.resolve(snapshot("available"));
    await second;
    initial.resolve(snapshot("connecting"));
    await first;
    expect(host.state().snapshot?.runtime.state).toBe("available");
    expect(host.state().loading).toBe(false);
  });

  test("a background read that supersedes loading still renders when data is unchanged", async () => {
    const initial = deferred<HostSnapshot>();
    const event = deferred<HostSnapshot>();
    const host = snapshotHost([initial.promise, event.promise], snapshot("available"));
    const first = host.refreshSnapshot();
    const second = host.refreshSnapshot(false);
    event.resolve(snapshot("available", 2));
    await second;
    expect(host.renders()).toBe(2);
    expect(host.state().loading).toBe(false);
    initial.resolve(snapshot("available"));
    await first;
    expect(host.renders()).toBe(2);
  });

  test("timestamp-only correction does not rebuild the sharing form", async () => {
    const host = snapshotHost([Promise.resolve(snapshot("available", 2))], snapshot("available"));
    await host.refreshSnapshot(false);
    expect(host.renders()).toBe(0);
  });

  test("repeated read failures do not keep rebuilding the form and recovery clears their notice", async () => {
    const error = new Error("snapshot unavailable");
    const host = snapshotHost([
      Promise.resolve().then(() => { throw error; }),
      Promise.resolve().then(() => { throw error; }),
      Promise.resolve(snapshot("available", 2)),
    ], snapshot("available"));
    await host.refreshSnapshot(false);
    await host.refreshSnapshot(false);
    expect(host.renders()).toBe(1);
    await host.refreshSnapshot(false);
    expect(host.renders()).toBe(2);
    expect(host.state().feedback).toBeNull();
  });
});
