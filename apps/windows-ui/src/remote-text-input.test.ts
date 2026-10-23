import { describe, expect, test } from "bun:test";
import { RemoteTextInput } from "./remote-text-input";

function key(key: string, extra: Record<string, unknown> = {}) {
  return {
    key, code: key, keyCode: 0, isComposing: false,
    ctrlKey: false, altKey: false, metaKey: false, ...extra,
  };
}

function host() {
  const sent: string[] = [];
  const actions: string[] = [];
  const state = new RemoteTextInput(
    (text) => { sent.push(text); actions.push(`text:${text}`); },
    () => actions.push("release-keys"),
  );
  return { state, sent, actions };
}

describe("local remote-desktop text input", () => {
  test("opening Process/229 and candidate keys remain local without waiting for compositionstart", () => {
    const { state, sent } = host();
    expect(state.keydown(key("Process", { code: "KeyN", keyCode: 229 }))).toBe(true);
    state.compositionstart();
    for (const value of [" ", "Enter", "Backspace", "Escape", "ArrowDown"]) {
      expect(state.keydown(key(value))).toBe(true);
    }
    state.compositionend("你");
    expect(state.keyup(key("Enter"))).toBe(true);
    expect(sent).toEqual(["你"]);
  });

  test("Chromium final input before compositionend commits exactly once", () => {
    const { state, sent, actions } = host();
    state.compositionstart();
    expect(state.input("zhong", "zhong", "insertCompositionText", true)).toBe(false);
    state.input("中文", "中文", "insertText", false);
    state.compositionend("中文");
    expect(sent).toEqual(["中文"]);
    expect(actions.slice(-2)).toEqual(["release-keys", "text:中文"]);
  });

  test("compositionend before a final input, including null data, does not duplicate text", () => {
    for (const data of ["中文", null]) {
      const { state, sent } = host();
      state.compositionstart();
      state.compositionend("中文");
      expect(state.input("中文", data, "insertText", false)).toBe(true);
      expect(sent).toEqual(["中文"]);
    }
  });

  test("empty compositionend cancels preedit rather than sending a textarea fallback", () => {
    const { state, sent } = host();
    state.compositionstart();
    state.input("zhong", "zhong", "insertCompositionText", true);
    state.compositionend("");
    state.input("zhong", null, "insertText", false);
    expect(sent).toEqual([]);
  });

  test("two successive identical candidates are separate commits", () => {
    const { state, sent } = host();
    for (let index = 0; index < 2; index += 1) {
      state.compositionstart();
      state.compositionend("中");
      state.input("中", "中", "insertText", false);
    }
    expect(sent).toEqual(["中", "中"]);
  });

  test("plain Latin and shifted punctuation use input once, while shortcuts stay physical", () => {
    const { state, sent } = host();
    for (const value of ["a", "A", "!", " "]) {
      expect(state.keydown(key(value, { code: "KeyA" }))).toBe(true);
      state.input(value, value, "insertText", false);
      expect(state.keyup(key(value, { code: "KeyA" }))).toBe(true);
    }
    expect(state.keydown(key("c", { ctrlKey: true }))).toBe(false);
    expect(state.keydown(key("Enter"))).toBe(false);
    expect(sent).toEqual(["a", "A", "!", " "]);
  });

  test("AltGraph characters and dead-key composition remain editable", () => {
    const { state, sent } = host();
    expect(state.keydown(key("@", { ctrlKey: true, altKey: true, getModifierState: () => true }))).toBe(true);
    state.input("@", "@", "insertText", false);
    expect(state.keydown(key("Dead"))).toBe(true);
    state.compositionstart();
    state.compositionend("é");
    expect(sent).toEqual(["@", "é"]);
  });

  test("reset invalidates late composition/input, then a fresh key or composition recovers", () => {
    const { state, sent } = host();
    state.compositionstart();
    state.reset();
    state.compositionend("丢弃");
    state.input("丢弃", "丢弃", "insertText", false);
    expect(sent).toEqual([]);
    state.keydown(key("a"));
    state.input("a", "a", "insertText", false);
    state.reset();
    state.focus();
    state.compositionstart();
    state.compositionend("恢复");
    expect(sent).toEqual(["a", "恢复"]);
  });

  test("refocusing after cancellation cannot revive an old composition tail", () => {
    for (const inputType of ["insertFromComposition", "insertText"]) {
      const { state, sent } = host();
      state.compositionstart();
      state.reset();
      state.focus();
      state.compositionend("旧字");
      state.input("旧字", "旧字", inputType, false);
      expect(sent).toEqual([]);
      state.compositionstart();
      state.compositionend("新字");
      expect(sent).toEqual(["新字"]);
    }
  });
});
