export type WindowsReleaseUnavailableReason =
  | "noRelease"
  | "invalidRelease"
  | "incompleteWindowsRelease"
  | "unverifiedWindowsRelease";

export type WindowsReleaseSource =
  | {
      kind: "release";
      latestVersion: string;
      publishedAt: string | null;
    }
  | {
      kind: "unavailable";
      reason: WindowsReleaseUnavailableReason;
    };

export type WindowsUpdateCheck =
  | {
      kind: "available";
      currentVersion: string;
      latestVersion: string;
      publishedAt: string | null;
    }
  | {
      kind: "current";
      currentVersion: string;
      latestVersion: string;
    }
  | {
      kind: "unavailable";
      currentVersion: string;
      reason: WindowsReleaseUnavailableReason;
    };

export interface WindowsUpdateProgress {
  stage: "checking" | "downloading" | "verifying" | "installing";
  version?: string;
  downloadedBytes?: number;
  totalBytes?: number;
}

export type WindowsUpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "error" }
  | WindowsUpdateCheck
  | { kind: "installing"; progress: WindowsUpdateProgress; cancelling: boolean; cancelError?: string }
  | { kind: "installError"; latestVersion: string; message: string };

interface WindowsUpdaterDependencies {
  version: () => Promise<string>;
  check: () => Promise<WindowsReleaseSource>;
  install: (progress: (value: WindowsUpdateProgress) => void) => Promise<void>;
  cancel: () => Promise<boolean>;
  changed: () => void;
  now?: () => number;
}

const UPDATE_CHECK_INTERVAL_MS = 4 * 60 * 60 * 1000;

/** Keeps background checks and a user-started installation from racing. */
export class WindowsUpdater {
  state: WindowsUpdateState = { kind: "idle" };
  private checkPending: Promise<void> | null = null;
  private installPending: Promise<void> | null = null;
  private cancelPending: Promise<void> | null = null;
  private revision = 0;
  private checkedAt: number | null = null;
  private checkIntervalMs = UPDATE_CHECK_INTERVAL_MS;
  private available: Extract<WindowsUpdateCheck, { kind: "available" }> | null = null;
  private dismissedVersion: string | null = null;
  private cancelAccepted = false;

  constructor(private readonly dependencies: WindowsUpdaterDependencies) {}

  get bannerVisible(): boolean {
    if (this.state.kind === "installing") return true;
    return (this.state.kind === "available" || this.state.kind === "installError")
      && this.dismissedVersion !== this.state.latestVersion;
  }

  check(force = false): Promise<void> {
    if (this.installPending || this.state.kind === "installing") return Promise.resolve();
    if (this.checkPending) return this.checkPending;
    const now = (this.dependencies.now ?? Date.now)();
    if (!force && this.checkedAt !== null && now - this.checkedAt < this.checkIntervalMs) return Promise.resolve();
    const revision = ++this.revision;
    this.checkedAt = now;
    const previous = this.state;
    const pending = Promise.resolve().then(async () => {
      try {
        const version = await this.dependencies.version();
        const result = evaluateWindowsRelease(version, await this.dependencies.check());
        if (revision !== this.revision) return;
        this.checkedAt = (this.dependencies.now ?? Date.now)();
        this.checkIntervalMs = result.kind === "unavailable" && result.reason !== "noRelease" ? 60_000 : UPDATE_CHECK_INTERVAL_MS;
        if (result.kind === "available") this.available = result;
        this.state = result;
      } catch {
        if (revision !== this.revision) return;
        this.checkedAt = (this.dependencies.now ?? Date.now)();
        this.checkIntervalMs = 60_000;
        this.state = previous.kind === "available" && !force ? previous : { kind: "error" };
      }
      this.dependencies.changed();
    }).finally(() => {
      if (this.checkPending === pending) this.checkPending = null;
    });
    this.checkPending = pending;
    if (force || previous.kind !== "available") {
      this.state = { kind: "checking" };
      this.dependencies.changed();
    }
    return pending;
  }

  install(): Promise<void> {
    if (this.installPending) return this.installPending;
    if (!this.available || (this.state.kind !== "available" && this.state.kind !== "installError")) return Promise.resolve();
    const revision = ++this.revision;
    this.cancelAccepted = false;
    this.dismissedVersion = null;
    this.state = { kind: "installing", progress: { stage: "checking", version: this.available.latestVersion }, cancelling: false };
    const pending = Promise.resolve().then(() => this.dependencies.install((progress) => {
      if (revision !== this.revision || this.state.kind !== "installing") return;
      this.state = { ...this.state, progress, cancelError: progress.stage === "downloading" ? this.state.cancelError : undefined };
      this.dependencies.changed();
    })).then(() => {
      if (revision !== this.revision || this.state.kind !== "installing") return;
      this.state = { ...this.state, progress: { ...this.state.progress, stage: "installing" }, cancelling: false };
      this.dependencies.changed();
    }, async (error: unknown) => {
      if (this.cancelPending) await this.cancelPending;
      if (revision !== this.revision || !this.available) return;
      this.state = this.cancelAccepted
        ? this.available
        : { kind: "installError", latestVersion: this.available.latestVersion,
          message: typeof error === "string" ? error : error instanceof Error ? error.message : "更新失败，请重试。" };
      this.dependencies.changed();
    }).finally(() => {
      if (this.installPending === pending) this.installPending = null;
    });
    this.installPending = pending;
    this.dependencies.changed();
    return pending;
  }

  cancel(): Promise<void> {
    if (this.cancelPending) return this.cancelPending;
    if (this.state.kind !== "installing" || this.state.progress.stage !== "downloading" || this.state.cancelling) return Promise.resolve();
    const revision = this.revision;
    const pending = Promise.resolve().then(() => this.dependencies.cancel()).then((accepted) => {
      if (revision !== this.revision) return;
      this.cancelAccepted = accepted;
      if (!accepted && this.state.kind === "installing") {
        this.state = { ...this.state, cancelling: false };
        this.dependencies.changed();
      }
    }, (error: unknown) => {
      if (revision !== this.revision || this.state.kind !== "installing") return;
      this.state = { ...this.state, cancelling: false,
        cancelError: typeof error === "string" ? error : error instanceof Error ? error.message : "取消失败，请重试。" };
      this.dependencies.changed();
    }).finally(() => {
      if (this.cancelPending === pending) this.cancelPending = null;
    });
    this.cancelPending = pending;
    this.state = { ...this.state, cancelling: true, cancelError: undefined };
    this.dependencies.changed();
    return pending;
  }

  dismiss(): void {
    if (this.state.kind !== "available" && this.state.kind !== "installError") return;
    this.dismissedVersion = this.state.latestVersion;
    this.dependencies.changed();
  }
}

export function windowsUpdateProgressLabel(progress: WindowsUpdateProgress, cancelling = false): string {
  if (cancelling) return "正在取消…";
  if (progress.stage === "checking") return "正在检查更新…";
  if (progress.stage === "verifying") return "正在校验…";
  if (progress.stage === "installing") return "正在安装…";
  const downloaded = Math.max(0, progress.downloadedBytes ?? 0);
  const total = progress.totalBytes ?? 0;
  if (total > 0 && Number.isFinite(total) && Number.isFinite(downloaded)) {
    return `正在下载 ${Math.min(100, Math.floor(downloaded / total * 100))}%`;
  }
  return downloaded > 0 && Number.isFinite(downloaded)
    ? `正在下载 ${(downloaded / 1024 / 1024).toFixed(1)} MB`
    : "正在下载…";
}

export function normalizeReleaseVersion(value: unknown): string | null {
  if (typeof value !== "string") {
    return null;
  }
  const match = /^v?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(
    value.trim(),
  );
  if (!match) {
    return null;
  }
  const parts = match.slice(1).map(Number);
  if (parts.some((part) => !Number.isSafeInteger(part))) {
    return null;
  }
  return parts.join(".");
}

export function compareReleaseVersions(left: string, right: string): number | null {
  const normalizedLeft = normalizeReleaseVersion(left);
  const normalizedRight = normalizeReleaseVersion(right);
  if (!normalizedLeft || !normalizedRight) {
    return null;
  }
  const leftParts = normalizedLeft.split(".").map(Number);
  const rightParts = normalizedRight.split(".").map(Number);
  for (let index = 0; index < leftParts.length; index += 1) {
    const difference = (leftParts[index] ?? 0) - (rightParts[index] ?? 0);
    if (difference !== 0) {
      return Math.sign(difference);
    }
  }
  return 0;
}

export function evaluateWindowsRelease(
  currentVersion: string,
  source: WindowsReleaseSource,
): WindowsUpdateCheck {
  const normalizedCurrent = normalizeReleaseVersion(currentVersion);
  if (!normalizedCurrent) {
    return {
      kind: "unavailable",
      currentVersion,
      reason: "invalidRelease",
    };
  }
  if (source.kind === "unavailable") {
    return {
      kind: "unavailable",
      currentVersion: normalizedCurrent,
      reason: source.reason,
    };
  }
  const latestVersion = normalizeReleaseVersion(source.latestVersion);
  const comparison = latestVersion
    ? compareReleaseVersions(latestVersion, normalizedCurrent)
    : null;
  if (!latestVersion || comparison === null) {
    return {
      kind: "unavailable",
      currentVersion: normalizedCurrent,
      reason: "invalidRelease",
    };
  }
  if (comparison <= 0) {
    return {
      kind: "current",
      currentVersion: normalizedCurrent,
      latestVersion,
    };
  }
  return {
    kind: "available",
    currentVersion: normalizedCurrent,
    latestVersion,
    publishedAt: source.publishedAt,
  };
}
