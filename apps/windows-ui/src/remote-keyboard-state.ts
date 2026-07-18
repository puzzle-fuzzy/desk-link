import { keyboardModifierMask } from "./remote-input";
import type { ControllerInput } from "./types";

export type ControllerKeyInput = ControllerInput & {
  kind: "key";
  key: string;
  pressed: boolean;
  modifiers: number;
};

export class RemoteKeyboardState {
  private readonly pressed = new Map<string, ControllerKeyInput>();

  modifierMask(): number {
    let mask = 0;
    for (const input of this.pressed.values()) {
      mask |= keyboardModifierMask(input.key);
    }
    return mask;
  }

  press(code: string, input: ControllerKeyInput): ControllerKeyInput[] {
    if (this.pressed.has(code)) {
      return [];
    }
    const ownModifier = keyboardModifierMask(input.key);
    const alreadyPressed = ownModifier !== 0 && (this.modifierMask() & ownModifier) !== 0;
    this.pressed.set(code, input);
    return alreadyPressed ? [] : [input];
  }

  release(code: string): ControllerKeyInput[] {
    const input = this.pressed.get(code);
    if (!input) {
      return [];
    }
    this.pressed.delete(code);
    const ownModifier = keyboardModifierMask(input.key);
    if (ownModifier !== 0 && (this.modifierMask() & ownModifier) !== 0) {
      return [];
    }
    return [{ ...input, pressed: false }];
  }

  releaseAll(): ControllerKeyInput[] {
    const releases: ControllerKeyInput[] = [];
    let releasedModifiers = 0;
    for (const input of [...this.pressed.values()].reverse()) {
      const ownModifier = keyboardModifierMask(input.key);
      if (ownModifier !== 0 && (releasedModifiers & ownModifier) !== 0) {
        continue;
      }
      releases.push({ ...input, pressed: false });
      releasedModifiers |= ownModifier;
    }
    this.pressed.clear();
    return releases;
  }
}
