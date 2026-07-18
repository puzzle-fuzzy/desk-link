import { describe, expect, test } from "bun:test";
import { RemoteKeyboardState, type ControllerKeyInput } from "./remote-keyboard-state";

function key(keyName: string, modifiers = 0): ControllerKeyInput {
  return { kind: "key", key: keyName, pressed: true, modifiers };
}

describe("remote keyboard pressed state", () => {
  test("keeps a modifier pressed until both physical sides are released", () => {
    const state = new RemoteKeyboardState();
    expect(state.press("ShiftLeft", key("shift"))).toEqual([key("shift")]);
    expect(state.press("ShiftRight", key("shift"))).toEqual([]);
    expect(state.modifierMask()).toBe(1);
    expect(state.release("ShiftLeft")).toEqual([]);
    expect(state.release("ShiftRight")).toEqual([{ ...key("shift"), pressed: false }]);
    expect(state.modifierMask()).toBe(0);
  });

  test("releases a shortcut with the same modifiers used on key down", () => {
    const state = new RemoteKeyboardState();
    expect(state.press("KeyA", key("character", 2))).toEqual([key("character", 2)]);
    expect(state.release("KeyA")).toEqual([{ ...key("character", 2), pressed: false }]);
  });

  test("releases main keys before modifiers and emits one release per modifier", () => {
    const state = new RemoteKeyboardState();
    state.press("ControlLeft", key("control"));
    state.press("ControlRight", key("control"));
    state.press("KeyA", key("character"));
    expect(state.releaseAll()).toEqual([
      { ...key("character"), pressed: false },
      { ...key("control"), pressed: false },
    ]);
    expect(state.releaseAll()).toEqual([]);
  });
});
