import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { keyboardKey, keyboardModifierMask, keyboardModifiers, mouseButton } from "./remote-input";
import { RemoteKeyboardState } from "./remote-keyboard-state";
import { isCompositionKey, RemoteTextInput } from "./remote-text-input";
import type { ControllerInput, RemoteInputContext } from "./types";

const source = readFileSync(resolve(import.meta.dir, "controller.ts"), "utf8");
const start = source.indexOf("function bindRemoteInput(");
const end = source.indexOf("\nasync function pasteLocalClipboardText(", start);
const escapeStart = source.indexOf("function handleRemoteFullscreenEscape(");
const escapeEnd = source.indexOf("\nfunction syncRemoteFullscreenControl(", escapeStart);
const panStart = source.indexOf("function setRemotePanMode(");
const panEnd = source.indexOf("\nfunction syncRemoteInteractionControls(", panStart);
const operations = new Bun.Transpiler({ loader: "ts" }).transformSync(
  source.slice(start, end) + source.slice(escapeStart, escapeEnd) + source.slice(panStart, panEnd),
);

class EventSurface {
  value = "";
  dataset: Record<string, string> = {};
  style: Record<string, string> = {};
  clientWidth = 100;
  clientHeight = 100;
  scrollLeft = 0;
  scrollTop = 0;
  focusCalls = 0;
  listeners = new Map<string, Array<(event: any) => void>>();
  addEventListener(name: string, listener: (event: any) => void) {
    const listeners = this.listeners.get(name) ?? [];
    listeners.push(listener);
    this.listeners.set(name, listeners);
  }
  emit(name: string, detail: Record<string, unknown> = {}) {
    const event = {
      key: "", code: "", keyCode: 0, isComposing: false, repeat: false,
      ctrlKey: false, altKey: false, metaKey: false, shiftKey: false,
      data: null, inputType: "", target: this, relatedTarget: null,
      defaultPrevented: false,
      preventDefault() { this.defaultPrevented = true; }, stopPropagation() {}, stopImmediatePropagation() {},
      ...detail,
    };
    for (const listener of this.listeners.get(name) ?? []) listener(event);
    return event;
  }
  focus() { this.focusCalls += 1; this.emit("focus"); }
  contains(target: unknown) { return target === this; }
  hasPointerCapture() { return false; }
  setPointerCapture() {}
  getBoundingClientRect() { return { left: 0, top: 0, width: 100, height: 100 }; }
}

function inputHost() {
  const viewport = new EventSurface();
  const carrier = new EventSurface();
  Object.assign(viewport, { querySelector: () => carrier, contains: (target: unknown) => target === viewport || target === carrier });
  const sent: ControllerInput[] = [];
  const texts: string[] = [];
  const fullscreenChanges: boolean[] = [];
  const contexts: RemoteInputContext[] = [];
  const moves: RemoteInputContext[] = [];
  let inputContext: RemoteInputContext | null = Object.freeze({ streamId: "9", inputEpoch: "1" });
  let nextFrame = 0;
  const frames = new Map<number, () => void>();
  const environment = {
    remotePanMode: false, remoteScaleMode: "actual", smartPasteBusy: false, pointerFrame: null,
    syncRemoteInteractionControls: () => {},
    pressedKeys: new RemoteKeyboardState(), pressedButtons: new Set(),
    heldKeyboardContext: null, heldButtonContext: null,
    currentInputContext: () => inputContext,
    keyboardKey, keyboardModifierMask, keyboardModifiers, mouseButton,
    remoteCanvasBounds: null, remoteViewportBounds: null,
    pointerInsideViewport: false,
    readRemoteCanvasBounds: () => viewport.getBoundingClientRect(),
    writePointerPosition: (_event: unknown, _canvas: unknown, point: { x: number; y: number }) => {
      point.x = 1; point.y = 1; return true;
    },
    fireMouseMove: (_x: number, _y: number, context: RemoteInputContext) => { moves.push(context); },
    document: { querySelector: () => null },
    window: { cancelAnimationFrame: (id: number) => { frames.delete(id); }, requestAnimationFrame: (callback: () => void) => { const id = ++nextFrame; frames.set(id, callback); return id; } },
    fireInput: (input: ControllerInput, context: RemoteInputContext) => { sent.push(input); contexts.push(context); },
    RemoteTextInput, isCompositionKey,
    snapshot: { runtime: { state: "connected", streamId: "9" } },
    inputDispatcher: {
      enqueueText: (text: string) => { texts.push(text); return Promise.resolve(); },
      discardPendingMoves: () => {},
      cancelShortcuts: () => {},
    },
    isRemoteClipboardPasteShortcut: () => false,
    remoteTextInput: null,
    remoteFullscreenActive: true,
    setRemoteFullscreen: (active: boolean) => { fullscreenChanges.push(active); return Promise.resolve(); },
  };
  const create = new Function(...Object.keys(environment), `${operations};
    bindRemoteInput(arguments[arguments.length - 2], arguments[arguments.length - 1]);
    return { handleRemoteFullscreenEscape, setRemotePanMode, resetRemoteInputState };
  `);
  const methods = create(...Object.values(environment), viewport, {});
  return { viewport, carrier, sent, texts, fullscreenChanges, contexts, moves,
    setContext: (context: RemoteInputContext | null) => { inputContext = context; },
    flushFrames: () => { const pending = [...frames.values()]; frames.clear(); pending.forEach((callback) => callback()); },
    reset: methods.resetRemoteInputState as () => void,
    escape: methods.handleRemoteFullscreenEscape as (event: unknown) => void,
    setPan: methods.setRemotePanMode as (active: boolean) => void,
  };
}

describe("remote desktop local IME integration", () => {
  test("commits Chinese from the editable carrier instead of dropping Process keys", () => {
    const host = inputHost();
    host.carrier.emit("focus");
    // Composition/input bubble from the editable carrier into the viewport.
    host.viewport.emit("compositionstart", { target: host.carrier });
    host.carrier.value = "中文";
    host.viewport.emit("input", {
      target: host.carrier, data: "中文", inputType: "insertCompositionText", isComposing: true,
    });
    host.viewport.emit("compositionend", { target: host.carrier, data: "中文" });
    expect(host.texts).toEqual(["中文"]);
  });

  test("candidate-selection Space does not reach the remote keyboard", () => {
    const host = inputHost();
    host.viewport.emit("compositionstart", { target: host.carrier });
    const event = host.viewport.emit("keydown", { key: " ", code: "Space", isComposing: true });
    expect(host.sent).toEqual([]);
    expect(event.defaultPrevented).toBe(false);
  });

  test("a normal pointer click and capture release retains the editable focus and IME", () => {
    const host = inputHost();
    host.viewport.emit("pointerdown", { button: 0, pointerId: 1, clientX: 30, clientY: 40 });
    host.viewport.emit("pointerup", { button: 0, pointerId: 1 });
    host.viewport.emit("lostpointercapture");
    host.viewport.emit("compositionstart", { target: host.carrier });
    host.viewport.emit("compositionend", { target: host.carrier, data: "中文" });
    expect(host.texts).toEqual(["中文"]);
    expect(host.carrier.style.left).toBe("30px");
    expect(host.carrier.style.top).toBe("40px");
    expect(host.sent.filter((input) => input.kind === "mouseButton").map((input) => input.pressed)).toEqual([true, false]);
  });

  test("Escape from local pan restores editable focus before typing without another click", () => {
    const host = inputHost();
    host.setPan(true);
    host.viewport.emit("keydown", { key: "Escape", code: "Escape" });
    expect(host.carrier.focusCalls).toBe(1);
    host.viewport.emit("compositionstart");
    host.viewport.emit("compositionend", { data: "继续输入" });
    expect(host.texts).toEqual(["继续输入"]);
    expect(host.sent).toEqual([]);
  });

  test("moving focus within the surface preserves composition; leaving discards late commits", () => {
    const host = inputHost();
    host.viewport.emit("compositionstart");
    host.viewport.emit("focusout", { relatedTarget: host.carrier });
    host.viewport.emit("compositionend", { data: "保留" });
    host.viewport.emit("compositionstart");
    host.carrier.value = "preedit";
    host.viewport.emit("focusout", { relatedTarget: {} });
    host.viewport.emit("compositionend", { data: "丢弃" });
    host.viewport.emit("input", { data: "丢弃", inputType: "insertText" });
    expect(host.texts).toEqual(["保留"]);
    expect(host.carrier.value).toBe("");
  });

  test("composition releases forwarded modifiers before committing Unicode", () => {
    const host = inputHost();
    host.viewport.emit("keydown", { key: "Control", code: "ControlLeft", ctrlKey: true });
    host.viewport.emit("compositionstart");
    host.viewport.emit("compositionend", { data: "你好" });
    expect(host.sent.map((input) => [input.key, input.pressed])).toEqual([["control", true], ["control", false]]);
    expect(host.texts).toEqual(["你好"]);
  });

  test("the document fullscreen shortcut leaves candidate Escape to the local IME", () => {
    const host = inputHost();
    host.viewport.emit("compositionstart");
    host.escape({ key: "Escape", repeat: false, isComposing: false, keyCode: 0 });
    expect(host.fullscreenChanges).toEqual([]);
    host.viewport.emit("compositionend", { data: "" });
    host.escape({ key: "Escape", repeat: false, isComposing: false, keyCode: 229 });
    expect(host.fullscreenChanges).toEqual([]);
    let prevented = false;
    host.escape({ key: "Escape", repeat: false, keyCode: 0, isComposing: false,
      preventDefault: () => { prevented = true; }, stopImmediatePropagation: () => {} });
    expect(host.fullscreenChanges).toEqual([false]);
    expect(prevented).toBe(true);
  });

  test("new input is paused until presentation while releases keep their original context", () => {
    const host = inputHost();
    host.viewport.emit("keydown", { key: "Control", code: "ControlLeft", ctrlKey: true });
    host.viewport.emit("pointerdown", { button: 0, pointerId: 1, clientX: 30, clientY: 40 });
    host.setContext(null);
    host.viewport.emit("keyup", { key: "Control", code: "ControlLeft" });
    host.viewport.emit("pointerup", { button: 0, pointerId: 1 });
    host.viewport.emit("keydown", { key: "Enter", code: "Enter" });
    host.viewport.emit("pointerdown", { button: 0, pointerId: 2, clientX: 30, clientY: 40 });
    host.viewport.emit("wheel", { deltaX: 0, deltaY: 100 });
    expect(host.sent.map((input) => [input.key ?? input.button, input.pressed])).toEqual([
      ["control", true], ["left", true], ["control", false], ["left", false],
    ]);
    expect(host.contexts).toEqual(Array.from({ length: 4 }, () => ({ streamId: "9", inputEpoch: "1" })));
  });

  test("pointer rAF cannot retag a captured old-frame position after a new epoch", () => {
    const host = inputHost();
    host.viewport.emit("pointermove", { clientX: 30, clientY: 40 });
    host.setContext(Object.freeze({ streamId: "9", inputEpoch: "2" }));
    host.flushFrames();
    expect(host.moves).toEqual([]);
    host.viewport.emit("pointermove", { clientX: 30, clientY: 40 });
    host.flushFrames();
    expect(host.moves).toEqual([{ streamId: "9", inputEpoch: "2" }]);
  });

  test("synthetic ReleaseAll uses the old context before new epoch input is admitted", () => {
    const host = inputHost();
    host.viewport.emit("keydown", { key: "Control", code: "ControlLeft", ctrlKey: true });
    host.setContext(null);
    host.reset();
    host.setContext(Object.freeze({ streamId: "9", inputEpoch: "2" }));
    host.viewport.emit("keyup", { key: "Control", code: "ControlLeft" });
    host.viewport.emit("keydown", { key: "Enter", code: "Enter" });
    expect(host.sent.map((input) => [input.key, input.pressed])).toEqual([["control", true], ["control", false], ["enter", true]]);
    expect(host.contexts.map((context) => context.inputEpoch)).toEqual(["1", "1", "2"]);
  });
});
