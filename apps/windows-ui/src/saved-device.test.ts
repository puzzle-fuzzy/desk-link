import { describe, expect, test } from "bun:test";

import { deviceIdsMatch, formatLastUsed } from "./saved-device";

describe("saved device presentation", () => {
  test("matches the same public device ID across display formats", () => {
    expect(deviceIdsMatch("123 456 789 012", "123-456-789-012")).toBe(true);
    expect(deviceIdsMatch("123 456 789 012", "987 654 321 098")).toBe(false);
    expect(deviceIdsMatch("", "")).toBe(false);
  });

  test("describes recent use with stable Chinese time buckets", () => {
    const now = 1_000_000;
    expect(formatLastUsed(now - 20, now)).toBe("刚刚");
    expect(formatLastUsed(now - 125, now)).toBe("2 分钟前");
    expect(formatLastUsed(now - 7_200, now)).toBe("2 小时前");
    expect(formatLastUsed(now - 259_200, now)).toBe("3 天前");
    expect(formatLastUsed(now + 30, now)).toBe("刚刚");
  });
});
