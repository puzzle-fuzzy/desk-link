import { describe, expect, test } from "bun:test";

import {
  compareReleaseVersions,
  evaluateWindowsRelease,
  normalizeReleaseVersion,
  WindowsUpdater,
  windowsUpdateProgressLabel,
  type WindowsReleaseSource,
  type WindowsUpdateProgress,
} from "./windows-update";

describe("Windows stable update presentation", () => {
  test("accepts only strict stable semantic versions", () => {
    expect(normalizeReleaseVersion("v0.1.42")).toBe("0.1.42");
    expect(normalizeReleaseVersion("1.12.3")).toBe("1.12.3");
    expect(normalizeReleaseVersion("v01.1.2")).toBeNull();
    expect(normalizeReleaseVersion("0.1.42-beta.1")).toBeNull();
    expect(normalizeReleaseVersion("latest")).toBeNull();
  });

  test("compares numeric components instead of sorting version text", () => {
    expect(compareReleaseVersions("0.1.10", "0.1.9")).toBe(1);
    expect(compareReleaseVersions("0.2.0", "0.10.0")).toBe(-1);
    expect(compareReleaseVersions("v1.0.0", "1.0.0")).toBe(0);
  });

  test("offers a newer release already verified by the Rust boundary", () => {
    expect(
      evaluateWindowsRelease("0.1.41", {
        kind: "release",
        latestVersion: "0.1.42",
        publishedAt: "2026-07-19T08:00:00.000Z",
      }),
    ).toEqual({
      kind: "available",
      currentVersion: "0.1.41",
      latestVersion: "0.1.42",
      publishedAt: "2026-07-19T08:00:00.000Z",
    });
  });

  test("does not call an equal or older release an update", () => {
    expect(
      evaluateWindowsRelease("0.1.42", {
        kind: "release",
        latestVersion: "0.1.42",
        publishedAt: null,
      }),
    ).toMatchObject({ kind: "current", latestVersion: "0.1.42" });
    expect(
      evaluateWindowsRelease("0.1.43", {
        kind: "release",
        latestVersion: "0.1.42",
        publishedAt: null,
      }),
    ).toMatchObject({ kind: "current", currentVersion: "0.1.43" });
  });

  test("preserves safe unavailable reasons from the Rust validator", () => {
    expect(
      evaluateWindowsRelease("0.1.41", {
        kind: "unavailable",
        reason: "unverifiedWindowsRelease",
      }),
    ).toEqual({
      kind: "unavailable",
      currentVersion: "0.1.41",
      reason: "unverifiedWindowsRelease",
    });
  });

  test("fails closed when either side reports an invalid version", () => {
    expect(
      evaluateWindowsRelease("development", {
        kind: "release",
        latestVersion: "0.1.42",
        publishedAt: null,
      }),
    ).toMatchObject({ kind: "unavailable", reason: "invalidRelease" });
    expect(
      evaluateWindowsRelease("0.1.41", {
        kind: "release",
        latestVersion: "0.1.42-beta.1",
        publishedAt: null,
      }),
    ).toMatchObject({ kind: "unavailable", reason: "invalidRelease" });
  });
});

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

async function flush(): Promise<void> {
  for (let index = 0; index < 12; index += 1) await Promise.resolve();
}

const release = (version = "0.1.93"): WindowsReleaseSource => ({ kind: "release", latestVersion: version, publishedAt: null });

function updaterHost() {
  let now = 100;
  let checks = 0;
  let installs = 0;
  let cancellations = 0;
  let changes = 0;
  let check = () => Promise.resolve(release());
  let cancel = () => Promise.resolve(true);
  const operations: Array<ReturnType<typeof deferred<void>>> = [];
  const progress: Array<(value: WindowsUpdateProgress) => void> = [];
  const updater = new WindowsUpdater({
    version: () => Promise.resolve("0.1.92"),
    check: () => { checks += 1; return check(); },
    install: (changed) => {
      installs += 1;
      progress.push(changed);
      const operation = deferred<void>();
      operations.push(operation);
      return operation.promise;
    },
    cancel: () => { cancellations += 1; return cancel(); },
    changed: () => { changes += 1; }, now: () => now,
  });
  return { updater, operations, progress,
    counts: () => ({ checks, installs, cancellations, changes }),
    advance: (milliseconds: number) => { now += milliseconds; },
    setCheck: (next: typeof check) => { check = next; },
    setCancel: (next: typeof cancel) => { cancel = next; },
  };
}

describe("Windows update workflow", () => {
  test("startup checks share one request and visible-window recovery is throttled", async () => {
    const host = updaterHost();
    const pending = deferred<WindowsReleaseSource>();
    host.setCheck(() => pending.promise);
    const first = host.updater.check();
    expect(host.updater.check()).toBe(first);
    expect(host.updater.bannerVisible).toBe(false);
    await flush();
    expect(host.counts().checks).toBe(1);
    pending.resolve(release());
    await first;
    expect(host.updater.bannerVisible).toBe(true);
    await host.updater.check();
    expect(host.counts().checks).toBe(1);
    host.advance(4 * 60 * 60 * 1000);
    await host.updater.check();
    expect(host.counts().checks).toBe(2);
  });

  test("no update and a failed background check do not show a banner; manual retry works", async () => {
    const host = updaterHost();
    host.setCheck(() => Promise.reject(new Error("offline")));
    await host.updater.check();
    expect(host.updater.state.kind).toBe("error");
    expect(host.updater.bannerVisible).toBe(false);
    host.setCheck(() => Promise.resolve(release("0.1.92")));
    await host.updater.check(true);
    expect(host.updater.state.kind).toBe("current");
    expect(host.updater.bannerVisible).toBe(false);
    host.setCheck(() => Promise.resolve(release()));
    await host.updater.check(true);
    expect(host.updater.bannerVisible).toBe(true);
  });

  test("offline startup and incomplete releases retry after one minute while no-release waits four hours", async () => {
    const host = updaterHost();
    host.setCheck(() => Promise.reject(new Error("offline")));
    await host.updater.check();
    host.setCheck(() => Promise.resolve({ kind: "unavailable", reason: "unverifiedWindowsRelease" }));
    host.advance(59_000);
    await host.updater.check();
    expect(host.counts().checks).toBe(1);
    host.advance(1_000);
    await host.updater.check();
    expect(host.counts().checks).toBe(2);
    host.advance(60_000);
    host.setCheck(() => Promise.resolve({ kind: "unavailable", reason: "noRelease" }));
    await host.updater.check();
    expect(host.counts().checks).toBe(3);
    host.advance(60_000);
    await host.updater.check();
    expect(host.counts().checks).toBe(3);
    host.advance(4 * 60 * 60 * 1000);
    await host.updater.check();
    expect(host.counts().checks).toBe(4);
  });

  test("dismissing a version keeps settings available and a different version can appear", async () => {
    const host = updaterHost();
    await host.updater.check();
    host.updater.dismiss();
    expect(host.updater.state.kind).toBe("available");
    expect(host.updater.bannerVisible).toBe(false);
    await host.updater.check(true);
    expect(host.updater.bannerVisible).toBe(false);
    host.setCheck(() => Promise.resolve(release("0.1.94")));
    await host.updater.check(true);
    expect(host.updater.bannerVisible).toBe(true);
  });

  test("install is single-flight and a late background check cannot replace its progress", async () => {
    const host = updaterHost();
    await host.updater.check();
    const old = deferred<WindowsReleaseSource>();
    host.setCheck(() => old.promise);
    host.advance(4 * 60 * 60 * 1000);
    const check = host.updater.check();
    await flush();
    const installing = host.updater.install();
    expect(host.updater.install()).toBe(installing);
    await flush();
    host.progress[0]!({ stage: "downloading", version: "0.1.93", downloadedBytes: 12, totalBytes: 30 });
    old.resolve(release("0.1.92"));
    await check;
    expect(host.updater.state).toMatchObject({ kind: "installing", progress: { stage: "downloading", downloadedBytes: 12 } });
    expect(host.counts().installs).toBe(1);
    host.operations[0]!.resolve();
    await installing;
    expect(host.updater.state).toMatchObject({ kind: "installing", progress: { stage: "installing" } });
  });

  test("an install error is visible and retry ignores the previous operation's late progress", async () => {
    const host = updaterHost();
    await host.updater.check();
    const first = host.updater.install();
    await flush();
    host.operations[0]!.reject("下载失败，请重试。");
    await first;
    expect(host.updater.state).toMatchObject({ kind: "installError", message: "下载失败，请重试。" });
    const retry = host.updater.install();
    await flush();
    host.progress[1]!({ stage: "verifying" });
    host.progress[0]!({ stage: "downloading", downloadedBytes: 1 });
    expect(host.updater.state).toMatchObject({ kind: "installing", progress: { stage: "verifying" } });
    host.operations[1]!.resolve();
    await retry;
  });

  test("download cancellation is single-flight and tolerates the native cancellation error arriving first", async () => {
    const host = updaterHost();
    await host.updater.check();
    const installing = host.updater.install();
    await flush();
    host.progress[0]!({ stage: "downloading", downloadedBytes: 1, totalBytes: 20 });
    const cancelled = deferred<boolean>();
    host.setCancel(() => cancelled.promise);
    const cancel = host.updater.cancel();
    expect(host.updater.cancel()).toBe(cancel);
    await flush();
    host.operations[0]!.reject("已取消下载");
    await flush();
    expect(host.updater.state).toMatchObject({ kind: "installing", cancelling: true });
    cancelled.resolve(true);
    await cancel;
    await installing;
    expect(host.updater.state.kind).toBe("available");
    expect(host.counts().cancellations).toBe(1);
  });

  test("cancel failure leaves the active operation usable, and verification cannot be cancelled", async () => {
    const host = updaterHost();
    await host.updater.check();
    const installing = host.updater.install();
    await flush();
    host.progress[0]!({ stage: "downloading" });
    host.setCancel(() => Promise.reject("无法取消，请重试。"));
    await host.updater.cancel();
    expect(host.updater.state).toMatchObject({ kind: "installing", cancelling: false, cancelError: "无法取消，请重试。" });
    host.progress[0]!({ stage: "verifying" });
    await host.updater.cancel();
    expect(host.counts().cancellations).toBe(1);
    host.operations[0]!.resolve();
    await installing;
  });

  test("a rejected cancellation handoff cannot hide a subsequent install error", async () => {
    const host = updaterHost();
    await host.updater.check();
    const installing = host.updater.install();
    await flush();
    host.progress[0]!({ stage: "downloading" });
    host.setCancel(() => Promise.resolve(false));
    await host.updater.cancel();
    expect(host.updater.state).toMatchObject({ kind: "installing", cancelling: false });
    host.operations[0]!.reject("安装失败，请重试。");
    await installing;
    expect(host.updater.state).toMatchObject({ kind: "installError", message: "安装失败，请重试。" });
  });

  test("download labels handle determinate, unknown-length and cancel progress", () => {
    expect(windowsUpdateProgressLabel({ stage: "downloading", downloadedBytes: 33, totalBytes: 100 })).toBe("正在下载 33%");
    expect(windowsUpdateProgressLabel({ stage: "downloading", downloadedBytes: 120, totalBytes: 100 })).toBe("正在下载 100%");
    expect(windowsUpdateProgressLabel({ stage: "downloading", downloadedBytes: 1024 * 1024 })).toBe("正在下载 1.0 MB");
    expect(windowsUpdateProgressLabel({ stage: "checking" })).toBe("正在检查更新…");
    expect(windowsUpdateProgressLabel({ stage: "verifying" })).toBe("正在校验…");
    expect(windowsUpdateProgressLabel({ stage: "installing" })).toBe("正在安装…");
    expect(windowsUpdateProgressLabel({ stage: "downloading" }, true)).toBe("正在取消…");
  });
});
