import { describe, expect, test } from "bun:test";

import {
  isRemoteClipboardPasteShortcut,
  type RemoteClipboardShortcutInput,
} from "./remote-clipboard-shortcut";

function shortcut(
  overrides: Partial<RemoteClipboardShortcutInput> = {},
): RemoteClipboardShortcutInput {
  return {
    altKey: false,
    code: "KeyV",
    ctrlKey: true,
    key: "v",
    metaKey: false,
    repeat: false,
    shiftKey: false,
    ...overrides,
  };
}

describe("remote clipboard paste shortcut", () => {
  test("accepts Ctrl+V and Ctrl+Shift+V keyboard events", () => {
    expect(isRemoteClipboardPasteShortcut(shortcut())).toBe(true);
    expect(isRemoteClipboardPasteShortcut(shortcut({ key: "V", shiftKey: true }))).toBe(true);
  });

  test("uses the physical key code when the active layout changes the key value", () => {
    expect(isRemoteClipboardPasteShortcut(shortcut({ key: "ν" }))).toBe(true);
  });

  test("rejects repeats and shortcuts without Ctrl", () => {
    expect(isRemoteClipboardPasteShortcut(shortcut({ repeat: true }))).toBe(false);
    expect(isRemoteClipboardPasteShortcut(shortcut({ ctrlKey: false }))).toBe(false);
  });

  test("does not capture AltGr, Meta, or unrelated shortcuts", () => {
    expect(isRemoteClipboardPasteShortcut(shortcut({ altKey: true }))).toBe(false);
    expect(isRemoteClipboardPasteShortcut(shortcut({ metaKey: true }))).toBe(false);
    expect(isRemoteClipboardPasteShortcut(shortcut({ code: "KeyC", key: "c" }))).toBe(false);
  });
});
