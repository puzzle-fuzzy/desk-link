import { describe, expect, test } from "bun:test";

import { clampWheel, keyboardKey, keyboardModifiers, mouseButton } from "./remote-input";

describe("remote keyboard mapping", () => {
  test("covers Windows navigation and function keys", () => {
    expect(keyboardKey("Delete")).toEqual({ key: "delete" });
    expect(keyboardKey("PageDown")).toEqual({ key: "pageDown" });
    expect(keyboardKey("F12")).toEqual({ key: "f12" });
  });

  test("keeps a single Unicode scalar as text input", () => {
    expect(keyboardKey("中")).toEqual({ key: "character", character: "中" });
    expect(keyboardKey("😀")).toEqual({ key: "character", character: "😀" });
    expect(keyboardKey("ab")).toBeNull();
  });

  test("encodes modifier bits consistently with the Rust protocol", () => {
    expect(keyboardModifiers({ shiftKey: true, ctrlKey: true, altKey: false, metaKey: true })).toBe(11);
  });
});

describe("remote pointer mapping", () => {
  test("accepts only the three supported mouse buttons", () => {
    expect(mouseButton(0)).toBe("left");
    expect(mouseButton(1)).toBe("middle");
    expect(mouseButton(2)).toBe("right");
    expect(mouseButton(4)).toBeNull();
  });

  test("bounds wheel bursts before crossing the IPC boundary", () => {
    expect(clampWheel(10_000)).toBe(1_200);
    expect(clampWheel(-10_000)).toBe(-1_200);
  });
});
