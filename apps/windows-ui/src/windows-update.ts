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
