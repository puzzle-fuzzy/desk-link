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

  test("rejects ambiguous password characters", () => {
    expect(deviceCredentialsAreValid("123 456 789 012", "AB1DEI3G")).toBe(false);
  });
});
