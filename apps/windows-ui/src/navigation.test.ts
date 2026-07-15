import { describe, expect, test } from "bun:test";

import { nextTabIndex } from "./navigation";

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
