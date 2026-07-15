export const MAX_POINTER_COORDINATE = 1_000_000;
export const MAX_WHEEL_DELTA = 1_200;

const NAMED_KEYS: Readonly<Record<string, string>> = {
  Enter: "enter",
  Escape: "escape",
  Backspace: "backspace",
  Tab: "tab",
  ArrowUp: "arrowUp",
  ArrowDown: "arrowDown",
  ArrowLeft: "arrowLeft",
  ArrowRight: "arrowRight",
  Delete: "delete",
  Insert: "insert",
  Home: "home",
  End: "end",
  PageUp: "pageUp",
  PageDown: "pageDown",
  CapsLock: "capsLock",
  F1: "f1",
  F2: "f2",
  F3: "f3",
  F4: "f4",
  F5: "f5",
  F6: "f6",
  F7: "f7",
  F8: "f8",
  F9: "f9",
  F10: "f10",
  F11: "f11",
  F12: "f12",
};

export function keyboardKey(value: string): { key: string; character?: string } | null {
  const named = NAMED_KEYS[value];
  if (named) {
    return { key: named };
  }
  return Array.from(value).length === 1 ? { key: "character", character: value } : null;
}

export function keyboardModifiers(event: Pick<KeyboardEvent, "shiftKey" | "ctrlKey" | "altKey" | "metaKey">): number {
  return Number(event.shiftKey)
    | (Number(event.ctrlKey) << 1)
    | (Number(event.altKey) << 2)
    | (Number(event.metaKey) << 3);
}

export function mouseButton(button: number): "left" | "right" | "middle" | null {
  return button === 0 ? "left" : button === 1 ? "middle" : button === 2 ? "right" : null;
}

export function clampWheel(value: number): number {
  return Math.max(-MAX_WHEEL_DELTA, Math.min(MAX_WHEEL_DELTA, value));
}
