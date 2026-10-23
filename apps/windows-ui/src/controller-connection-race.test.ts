import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { mergeControllerOperationSnapshot } from "./controller-snapshot";
import type { ControllerSnapshot } from "./types";

const source = readFileSync(resolve(import.meta.dir, "controller.ts"), "utf8");
const start = source.indexOf("async function beginConnection(");
const end = source.indexOf("\nasync function removeStoredDevice(", start);
if (start < 0 || end < 0) throw new Error("Unable to locate controller connection lifecycle");
const operations = new Bun.Transpiler({ loader: "ts" }).transformSync(source.slice(start, end));

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((ok) => { resolve = ok; });
  return { promise, resolve };
}

async function flushPromises(): Promise<void> {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
}

function snapshot(state: ControllerSnapshot["runtime"]["state"]): ControllerSnapshot {
  return {
    runtime: { state, title: state, detail: state, streamId: state === "connected" ? 9 : null },
    savedConnection: null, connectionError: null, savedDevices: [], savedDevicesError: null,
    fileRecovery: null, fileRecoveryError: null, fileQueueRecovery: null, fileQueueRecoveryError: null,
  };
}

function controllerHost() {
  const recovery = deferred<ControllerSnapshot>();
  const disconnect = deferred<ControllerSnapshot>();
  let disconnects = 0;
  const environment = {
    snapshot: snapshot("finding"), channelGeneration: 0, runtimeSignalRevision: 0,
    busy: false, cancelling: false, feedback: null, forgetConfirmation: null,
    clearRenameDraft: () => {}, attemptStartedAtMs: null, attemptTarget: "",
    remoteSurfaceState: "empty", videoConfig: null, videoSequenceHeader: new Uint8Array(), failedVideoConfig: null,
    remoteAudio: { release: () => {}, setEnabled: () => {}, prepare: () => {}, push: () => {}, resetConnection: () => {} },
    audioStatus: "starting", audioEnabled: true, audioToggleBusy: false, audioMessage: "",
    resetVideoTelemetry: () => {}, prepareControllerRender: () => {},
    createControllerChannels: () => ({}), activeChannels: null, requestRender: () => {}, handleSignal: () => {},
    mergeControllerOperationSnapshot,
    fileRecoveryAvailable: false, fileTransfer: null, isFileTransferRetryable: () => false,
    transferPanelOpen: false, deviceIdsMatch: (left: string, right: string) => left === right,
    getControllerSnapshot: () => recovery.promise, normalizeError: (error: unknown) => (error as Error).message,
    isActiveConnectionState: (state: string) => ["finding", "connecting", "waitingApproval", "reconnecting"].includes(state),
    clearConnectionAttempt: () => {}, remoteFullscreenActive: false, remoteFullscreenDesired: false,
    setRemoteFullscreen: () => Promise.resolve(),
    disconnectController: () => { disconnects += 1; return disconnect.promise; },
    deviceIdDraft: "123 456 789 012", temporaryPasswordDraft: "ABCDEFGH",
  };
  const create = new Function(...Object.keys(environment), `${operations}
    return {
      beginConnection, cancelConnection,
      newerSession(next) { channelGeneration += 1; snapshot = next; busy = false; },
      status(next) { runtimeSignalRevision += 1; snapshot = next; },
      state: () => ({ snapshot, busy, cancelling, deviceIdDraft, temporaryPasswordDraft })
    };
  `);
  const host = create(...Object.values(environment)) as {
    beginConnection(operation: () => Promise<ControllerSnapshot>, target: string): Promise<boolean>;
    cancelConnection(): Promise<void>;
    newerSession(next: ControllerSnapshot): void;
    status(next: ControllerSnapshot): void;
    state(): { snapshot: ControllerSnapshot; busy: boolean; cancelling: boolean; deviceIdDraft: string; temporaryPasswordDraft: string };
  };
  return { ...host, recovery, disconnect, disconnects: () => disconnects };
}

describe("controller connection and cancellation ordering", () => {
  test("a failed attempt's late recovery read cannot overwrite a cancelled or newer session", async () => {
    const host = controllerHost();
    const pending = host.beginConnection(async () => { throw new Error("connection failed"); }, "fixture");
    await flushPromises();
    host.newerSession(snapshot("idle"));
    host.recovery.resolve(snapshot("finding"));
    expect(await pending).toBe(false);
    expect(host.state().snapshot.runtime.state).toBe("idle");
    expect(host.state().busy).toBe(false);
  });

  test("status arriving during an error recovery read takes priority over its older snapshot", async () => {
    const host = controllerHost();
    const pending = host.beginConnection(async () => { throw new Error("connection failed"); }, "fixture");
    await flushPromises();
    host.status(snapshot("connected"));
    host.recovery.resolve(snapshot("stopped"));
    await pending;
    expect(host.state().snapshot.runtime.state).toBe("connected");
    expect(host.state().snapshot.runtime.streamId).toBe(9);
  });

  test("cancel is serialized and retains the typed credentials for retry", async () => {
    const host = controllerHost();
    const pending = host.cancelConnection();
    await host.cancelConnection();
    expect(host.disconnects()).toBe(1);
    expect(await host.beginConnection(() => Promise.resolve(snapshot("connected")), "fixture")).toBe(false);
    host.disconnect.resolve(snapshot("idle"));
    await pending;
    expect(host.state().snapshot.runtime.state).toBe("idle");
    expect(host.state().cancelling).toBe(false);
    expect(host.state().deviceIdDraft).toBe("123 456 789 012");
    expect(host.state().temporaryPasswordDraft).toBe("ABCDEFGH");
  });
});
