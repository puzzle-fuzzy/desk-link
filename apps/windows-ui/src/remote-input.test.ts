import { describe, expect, test } from "bun:test";

import {
  clampWheel,
  keyboardKey,
  keyboardModifierMask,
  keyboardModifiers,
  mouseButton,
  normalizedPointerPosition,
  remoteCursorContentPosition,
} from "./remote-input";

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
    expect(keyboardModifierMask("control")).toBe(2);
    expect(keyboardModifierMask("meta")).toBe(8);
    expect(keyboardModifiers(
      { shiftKey: true, ctrlKey: true, altKey: false, metaKey: true },
      keyboardModifierMask("control") | keyboardModifierMask("meta"),
    )).toBe(1);
  });

  test("maps standalone modifiers so mouse operations can keep them pressed", () => {
    expect(keyboardKey("Control")).toEqual({ key: "control" });
    expect(keyboardKey("Alt")).toEqual({ key: "alt" });
    expect(keyboardKey("Shift")).toEqual({ key: "shift" });
    expect(keyboardKey("Meta")).toEqual({ key: "meta" });
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

  test("maps the actual canvas instead of the surrounding letterbox", () => {
    const canvas = { left: 240, top: 90, width: 960, height: 540 };
    expect(normalizedPointerPosition(240, 90, canvas)).toEqual({ x: 0, y: 0 });
    expect(normalizedPointerPosition(720, 360, canvas)).toEqual({ x: 500_000, y: 500_000 });
    expect(normalizedPointerPosition(1_200, 630, canvas)).toEqual({ x: 1_000_000, y: 1_000_000 });
    expect(normalizedPointerPosition(200, 360, canvas)).toBeNull();
  });

  test("keeps fractional high-DPI canvas bounds accurate", () => {
    const canvas = { left: 18.25, top: 42.5, width: 1365.5, height: 768.25 };
    expect(normalizedPointerPosition(701, 426.625, canvas)).toEqual({
      x: 500_000,
      y: 500_000,
    });
  });

  test("keeps the remote cursor aligned while the 1:1 canvas is scrolled", () => {
    const position = remoteCursorContentPosition(
      500_000,
      250_000,
      { left: -180, top: -60, width: 1920, height: 1080 },
      { left: 100, top: 40 },
      300,
      120,
    );
    expect(position).toEqual({ left: 980, top: 290 });
    expect(position.left - 300 + 100).toBe(-180 + 960);
    expect(position.top - 120 + 40).toBe(-60 + 270);
  });
});
