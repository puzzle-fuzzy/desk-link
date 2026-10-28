import { describe, expect, test } from "bun:test";

import {
  deviceCredentialsAreValid,
  formatDeviceId,
  normalizeTemporaryPassword,
} from "./device-credentials";

describe("formatDeviceId", () => {
  test("keeps twelve digits and groups them for reading", () => {
    expect(formatDeviceId("123-456 78901234")).toBe("123 456 789 012");
  });

  test("ignores non-digit input", () => {
    expect(formatDeviceId("设备 123abc456")).toBe("123 456");
  });
});

describe("normalizeTemporaryPassword", () => {
  test("uppercases and removes separators", () => {
    expect(normalizeTemporaryPassword("ab2d-ef3g")).toBe("AB2DEF3G");
  });
});

describe("deviceCredentialsAreValid", () => {
  test("accepts a complete ID and non-ambiguous password", () => {
    expect(deviceCredentialsAreValid("123 456 789 012", "AB2DEF3G")).toBe(true);
  });

  test("accepts user-chosen fixed passwords with any characters", () => {
    // Confusable characters such as 0, 1, I and O are ordinary password
    // characters for user-chosen fixed passwords.
    expect(deviceCredentialsAreValid("123 456 789 012", "AB1DEI3G")).toBe(true);
    expect(deviceCredentialsAreValid("123 456 789 012", "My0Passw0rd-2026!")).toBe(true);
    expect(deviceCredentialsAreValid("123 456 789 012", "correct horse:电池 🐎!")).toBe(true);
  });

  test("rejects passwords outside the shared byte bounds", () => {
    expect(deviceCredentialsAreValid("123 456 789 012", "short")).toBe(false);
    expect(deviceCredentialsAreValid("123 456 789 012", "a".repeat(65))).toBe(false);
    // 42 three-byte characters exceed the 64-byte bound even though the
    // string length itself looks short.
    expect(deviceCredentialsAreValid("123 456 789 012", "密".repeat(22))).toBe(false);
  });
});
