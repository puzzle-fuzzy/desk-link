import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";

import { canRecoverWorkspaceSnapshot } from "./account-mode";
import { LatestRequest } from "./latest-request";
import type { FixedAccessSummary, HostSnapshot, PairingSessionSummary } from "./types";

const source = readFileSync(resolve(import.meta.dir, "main.ts"), "utf8");
function operationSource(startMarker: string, endMarker: string): string {
  const start = source.indexOf(startMarker);
  const end = source.indexOf(endMarker, start);
  if (start < 0 || end < 0) throw new Error("Unable to locate sharing credential operation");
  return new Bun.Transpiler({ loader: "ts" }).transformSync(source.slice(start, end));
}
const operations = [
  operationSource("function clearPairingSecrets()", "\nasync function loadPreferences()"),
  operationSource("async function beginPairing()", "\nasync function copyCredential("),
  operationSource("function updateCredentialExpiry()", "\nvoid bootstrap();"),
].join("\n");

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

async function flushPromises(): Promise<void> {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
}

function sharingHost(overrides: Record<string, unknown> = {}) {
  let renders = 0;
  let refreshes = 0;
  const fixedReads = [deferred<FixedAccessSummary>(), deferred<FixedAccessSummary>()];
  let fixedReadCount = 0;
  const fixedAccessRequests = new LatestRequest();
  const snapshotRequests = new LatestRequest();
  const snapshot = {
    connection: {}, trustedError: null, fixedPasswordEnabled: true, fixedPasswordError: null,
    pairingActive: false, runtime: { state: "available" }, pendingApproval: null,
  } as unknown as HostSnapshot;
  const environment = {
    snapshot, snapshotRequests, fixedAccessRequests,
    fixedAccess: null, fixedAccessBusy: false, fixedAccessConfirmation: null,
    pairingSession: null, pairingBusy: false, feedback: null, activeView: "fixedAccess",
    account: { signedIn: false }, accountLocalMode: true, accountBusy: false,
    canRecoverWorkspaceSnapshot,
    getFixedAccessPassword: () => fixedReads[fixedReadCount++]!.promise,
    regenerateFixedAccessPassword: () => Promise.resolve({ deviceId: "fixture", password: "new-secret" }),
    disableFixedAccessPassword: () => Promise.resolve({
      ...snapshot, fixedPasswordEnabled: false, runtime: { ...snapshot.runtime, state: "connecting" },
    }),
    startPairingSession: () => Promise.resolve({ deviceId: "fixture", temporaryPassword: "temporary", qrSvg: "private-invite", expiresAtUnixS: 200 }),
    cancelPairingSession: () => Promise.resolve({ ...snapshot, runtime: { ...snapshot.runtime, state: "connecting" } }),
    refreshSnapshot: async () => { refreshes += 1; },
    render: () => { renders += 1; }, normalizeError: (error: unknown) => (error as Error).message,
    Date: { now: () => 100_000 },
    document: { querySelector: () => null, querySelectorAll: () => [] },
    formatPairingRemaining: () => "", formatApprovalRemaining: () => "", focusNewApproval: () => {},
    expiredApprovalId: null,
    ...overrides,
  };
  const create = new Function(...Object.keys(environment), `${operations}
    return {
      loadFixedAccess, regenerateFixedAccess, disableFixedAccess, beginPairing, cancelPairing, updateCredentialExpiry,
      leave(view) { clearFixedAccessSecrets(); activeView = view; },
      setPairing(session) { pairingSession = session; },
      setView(view) { activeView = view; },
      state: () => ({ fixedAccess, fixedAccessBusy, pairingSession, snapshot, activeView, feedback })
    };
  `);
  const host = create(...Object.values(environment)) as {
    loadFixedAccess(): Promise<void>; regenerateFixedAccess(): Promise<void>; disableFixedAccess(): Promise<void>;
    beginPairing(): Promise<void>; cancelPairing(): Promise<void>; updateCredentialExpiry(): void;
    leave(view: string): void; setView(view: string): void; setPairing(session: PairingSessionSummary): void;
    state(): { fixedAccess: FixedAccessSummary | null; fixedAccessBusy: boolean; pairingSession: PairingSessionSummary | null; snapshot: HostSnapshot; activeView: string; feedback: unknown };
  };
  return { ...host, fixedReads, snapshotRequests, fixedReadCount: () => fixedReadCount, renders: () => renders, refreshes: () => refreshes };
}

describe("sharing credential lifecycle", () => {
  test("does not restore a fixed password after leaving its page", async () => {
    const host = sharingHost();
    const pending = host.loadFixedAccess();
    host.leave("connection");
    const secret = { deviceId: "fixture", password: "old-secret" };
    host.fixedReads[0]!.resolve(secret);
    await pending;
    expect(secret.password).toBe("");
    expect(host.state().fixedAccess).toBeNull();
    expect(host.state().fixedAccessBusy).toBe(false);
  });

  test("returning while an old read is pending obtains a new secret without getting stuck", async () => {
    const host = sharingHost();
    const pending = host.loadFixedAccess();
    host.leave("connection");
    host.setView("fixedAccess");
    await host.loadFixedAccess();
    const stale = { deviceId: "fixture", password: "old-secret" };
    host.fixedReads[0]!.resolve(stale);
    await pending;
    expect(host.fixedReadCount()).toBe(2);
    expect(stale.password).toBe("");
    host.fixedReads[1]!.resolve({ deviceId: "fixture", password: "current-secret" });
    await flushPromises();
    expect(host.state().fixedAccess?.password).toBe("current-secret");
    expect(host.state().fixedAccessBusy).toBe(false);
  });

  test("a failed fixed-password read can be explicitly retried", async () => {
    const host = sharingHost();
    const failed = host.loadFixedAccess();
    host.fixedReads[0]!.reject(new Error("temporary read failure"));
    await failed;
    expect(host.state().fixedAccessBusy).toBe(false);
    const retried = host.loadFixedAccess();
    host.fixedReads[1]!.resolve({ deviceId: "fixture", password: "current-secret" });
    await retried;
    expect(host.state().fixedAccess?.password).toBe("current-secret");
  });

  test("a completed fixed-password change does not reveal its secret on a different page", async () => {
    const generated = deferred<FixedAccessSummary>();
    const host = sharingHost({ regenerateFixedAccessPassword: () => generated.promise });
    const pending = host.regenerateFixedAccess();
    host.leave("controller");
    const secret = { deviceId: "fixture", password: "new-secret" };
    generated.resolve(secret);
    await pending;
    expect(secret.password).toBe("");
    expect(host.state().fixedAccess).toBeNull();
    expect(host.state().snapshot.fixedPasswordEnabled).toBe(true);
    expect(host.refreshes()).toBe(1);
  });

  test("disabling fixed access preserves newer live host runtime and invalidates older reads", async () => {
    const host = sharingHost();
    const olderRead = host.snapshotRequests.begin();
    await host.disableFixedAccess();
    expect(host.snapshotRequests.isCurrent(olderRead)).toBe(false);
    expect(host.state().snapshot.fixedPasswordEnabled).toBe(false);
    expect(host.state().snapshot.runtime.state).toBe("available");
    expect(host.refreshes()).toBe(1);
  });

  test("expired temporary passwords and QR invitations are cleared away from the pairing page", () => {
    const host = sharingHost({ activeView: "settings" });
    const session = { deviceId: "fixture", temporaryPassword: "expired", qrSvg: "private-invite", expiresAtUnixS: 99 };
    host.setPairing(session);
    host.updateCredentialExpiry();
    expect(session.temporaryPassword).toBe("");
    expect(session.qrSvg).toBe("");
    expect(host.state().pairingSession).toBeNull();
    expect(host.renders()).toBe(0);
    expect(host.refreshes()).toBe(1);
  });

  test("temporary-password generation does not pull a user back after they navigate away", async () => {
    const generated = deferred<PairingSessionSummary>();
    const host = sharingHost({ activeView: "connection", startPairingSession: () => generated.promise });
    const olderRead = host.snapshotRequests.begin();
    const pending = host.beginPairing();
    host.leave("settings");
    generated.resolve({ deviceId: "fixture", temporaryPassword: "temporary", qrSvg: "private-invite", expiresAtUnixS: 200 });
    await pending;
    expect(host.state().activeView).toBe("settings");
    expect(host.snapshotRequests.isCurrent(olderRead)).toBe(false);
    expect(host.state().snapshot.pairingActive).toBe(true);
    expect(host.refreshes()).toBe(1);
  });

  test("ending temporary access returns to sharing without replacing newer runtime state", async () => {
    const host = sharingHost({ activeView: "pairing" });
    const session = { deviceId: "fixture", temporaryPassword: "temporary", qrSvg: "private-invite", expiresAtUnixS: 200 };
    host.setPairing(session);
    await host.cancelPairing();
    expect(session.temporaryPassword).toBe("");
    expect(session.qrSvg).toBe("");
    expect(host.state().activeView).toBe("connection");
    expect(host.state().snapshot.pairingActive).toBe(false);
    expect(host.state().snapshot.runtime.state).toBe("available");
  });
});
