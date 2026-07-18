import { describe, expect, test } from "bun:test";

import {
  DESKTOP_NAV_ITEMS,
  navigationViewFor,
  nextTabIndex,
} from "./navigation";

test("uses remote tasks as the shared desktop navigation", () => {
  expect(DESKTOP_NAV_ITEMS.map((item) => item.label)).toEqual([
    "连接设备",
    "共享此设备",
    "已批准设备",
    "设置 / 诊断",
  ]);
});

test("keeps pairing and fixed access as secondary pages", () => {
  expect(navigationViewFor("pairing")).toBe("connection");
  expect(navigationViewFor("fixedAccess")).toBe("settings");
});

describe("keyboard tab navigation", () => {
  test("moves between adjacent tabs and wraps at both ends", () => {
    expect(nextTabIndex(0, 4, "ArrowRight")).toBe(1);
    expect(nextTabIndex(3, 4, "ArrowRight")).toBe(0);
    expect(nextTabIndex(0, 4, "ArrowLeft")).toBe(3);
  });

  test("supports Home and End without consuming unrelated keys", () => {
    expect(nextTabIndex(2, 4, "Home")).toBe(0);
    expect(nextTabIndex(1, 4, "End")).toBe(3);
    expect(nextTabIndex(1, 4, "Enter")).toBeNull();
  });
});
