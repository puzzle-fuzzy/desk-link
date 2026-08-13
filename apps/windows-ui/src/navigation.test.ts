import { describe, expect, test } from "bun:test";

import {
  DESKTOP_NAV_ITEMS,
  navigationViewFor,
  nextMenuIndex,
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

describe("utility menu keyboard navigation", () => {
  test("moves vertically and wraps at both ends", () => {
    expect(nextMenuIndex(0, 5, "ArrowDown")).toBe(1);
    expect(nextMenuIndex(4, 5, "ArrowDown")).toBe(0);
    expect(nextMenuIndex(0, 5, "ArrowUp")).toBe(4);
  });

  test("starts from the first or last item when the menu has just opened", () => {
    expect(nextMenuIndex(-1, 5, "ArrowDown")).toBe(0);
    expect(nextMenuIndex(-1, 5, "ArrowUp")).toBe(4);
    expect(nextMenuIndex(-1, 5, "Home")).toBe(0);
    expect(nextMenuIndex(-1, 5, "End")).toBe(4);
    expect(nextMenuIndex(-1, 5, "Enter")).toBeNull();
  });
});
