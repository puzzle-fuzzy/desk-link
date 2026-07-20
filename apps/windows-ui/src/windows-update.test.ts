import { describe, expect, test } from "bun:test";

import {
  compareReleaseVersions,
  evaluateWindowsRelease,
  normalizeReleaseVersion,
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
