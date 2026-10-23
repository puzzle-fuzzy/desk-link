import { describe, expect, test } from "bun:test";

import { approvalNeedsFocus, approvalTabFocusIndex } from "./approval-focus";

describe("host approval keyboard focus", () => {
  test("keeps focus on the dialog while busy or expired actions are disabled", () => {
    expect(approvalTabFocusIndex(0, -1, false)).toBe(-1);
    expect(approvalTabFocusIndex(0, -1, true)).toBe(-1);
  });

  test("cycles both directions at the approval controls' boundaries", () => {
    expect(approvalTabFocusIndex(2, 1, false)).toBe(0);
    expect(approvalTabFocusIndex(2, 0, true)).toBe(1);
    expect(approvalTabFocusIndex(1, 0, false)).toBe(0);
    expect(approvalTabFocusIndex(1, 0, true)).toBe(0);
  });

  test("allows ordinary tab navigation within the dialog", () => {
    expect(approvalTabFocusIndex(2, 0, false)).toBeNull();
    expect(approvalTabFocusIndex(2, 1, true)).toBeNull();
  });

  test("recaptures focus from the dialog container or background", () => {
    expect(approvalTabFocusIndex(2, -1, false)).toBe(0);
    expect(approvalTabFocusIndex(2, -1, true)).toBe(1);
  });

  test("refocuses an unchanged request after rendering replaced its controls", () => {
    expect(approvalNeedsFocus(3, 3, false)).toBe(true);
    expect(approvalNeedsFocus(3, 3, true)).toBe(false);
    expect(approvalNeedsFocus(4, 3, true)).toBe(true);
    expect(approvalNeedsFocus(3, null, false)).toBe(true);
  });
});
