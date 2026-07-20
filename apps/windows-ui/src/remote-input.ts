export const MAX_POINTER_COORDINATE = 1_000_000;
export const MAX_WHEEL_DELTA = 1_200;

export type PointerBounds = Pick<DOMRectReadOnly, "left" | "top" | "width" | "height">;

export function containedPointerBounds(
  bounds: PointerBounds,
  contentWidth: number,
  contentHeight: number,
): PointerBounds {
  if (bounds.width <= 0 || bounds.height <= 0 || contentWidth <= 0 || contentHeight <= 0) {
    return { left: bounds.left, top: bounds.top, width: 0, height: 0 };
  }
  const scale = Math.min(bounds.width / contentWidth, bounds.height / contentHeight);
  const width = contentWidth * scale;
  const height = contentHeight * scale;
  return {
    left: bounds.left + (bounds.width - width) / 2,
    top: bounds.top + (bounds.height - height) / 2,
    width,
    height,
  };
}

export function normalizedPointerPosition(
  clientX: number,
  clientY: number,
  bounds: PointerBounds,
): { x: number; y: number } | null {
  const right = bounds.left + bounds.width;
  const bottom = bounds.top + bounds.height;
  if (
    bounds.width <= 0
    || bounds.height <= 0
    || clientX < bounds.left
    || clientX > right
    || clientY < bounds.top
    || clientY > bottom
  ) {
    return null;
  }
  const x = Math.max(0, Math.min(1, (clientX - bounds.left) / bounds.width));
  const y = Math.max(0, Math.min(1, (clientY - bounds.top) / bounds.height));
  return {
    x: Math.round(x * MAX_POINTER_COORDINATE),
    y: Math.round(y * MAX_POINTER_COORDINATE),
  };
}

export function remoteCursorContentPosition(
  x: number,
  y: number,
  canvasBounds: PointerBounds,
  viewportBounds: Pick<PointerBounds, "left" | "top">,
  scrollLeft = 0,
  scrollTop = 0,
): { left: number; top: number } {
  return {
    left: canvasBounds.left - viewportBounds.left + scrollLeft
      + (x / MAX_POINTER_COORDINATE) * canvasBounds.width,
    top: canvasBounds.top - viewportBounds.top + scrollTop
      + (y / MAX_POINTER_COORDINATE) * canvasBounds.height,
  };
}

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
  Control: "control",
  Alt: "alt",
  Shift: "shift",
  Meta: "meta",
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

const MODIFIER_BITS: Readonly<Record<string, number>> = {
  shift: 1,
  control: 1 << 1,
  alt: 1 << 2,
  meta: 1 << 3,
};

export function keyboardKey(value: string): { key: string; character?: string } | null {
  const named = NAMED_KEYS[value];
  if (named) {
    return { key: named };
  }
  return Array.from(value).length === 1 ? { key: "character", character: value } : null;
}

export function keyboardModifierMask(key: string | undefined): number {
  return key ? (MODIFIER_BITS[key] ?? 0) : 0;
}

export function keyboardModifiers(
  event: Pick<KeyboardEvent, "shiftKey" | "ctrlKey" | "altKey" | "metaKey">,
  excluded = 0,
): number {
  const modifiers = Number(event.shiftKey)
    | (Number(event.ctrlKey) << 1)
    | (Number(event.altKey) << 2)
    | (Number(event.metaKey) << 3);
  return modifiers & ~excluded & 0x0f;
}

export function mouseButton(button: number): "left" | "right" | "middle" | null {
  return button === 0 ? "left" : button === 1 ? "middle" : button === 2 ? "right" : null;
}

export function clampWheel(value: number): number {
  return Math.max(-MAX_WHEEL_DELTA, Math.min(MAX_WHEEL_DELTA, value));
}
