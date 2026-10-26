import { describe, expect, test } from "bun:test";
import { RemoteInputDispatcher } from "./remote-input-dispatcher";
import type { ControllerInput, RemoteInputContext } from "./types";

const oldContext = { streamId: "41", inputEpoch: "7" };
const newContext = { streamId: "41", inputEpoch: "8" };
const blockingKey: ControllerInput = { kind: "key", key: "shift", pressed: true, modifiers: 0 };

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((next) => { resolve = next; });
  return { promise, resolve };
}

function blockedSender() {
  const first = deferred();
  const sent: Array<{ input: ControllerInput; context: RemoteInputContext }> = [];
  const dispatcher = new RemoteInputDispatcher(async (input, context) => {
    sent.push({ input: { ...input }, context: { ...context } });
    if (sent.length === 1) await first.promise;
  });
  return { first, sent, dispatcher };
}

describe("输入队列保留产生事件时的epoch", () => {
  test("超过2^53的相邻stream和epoch不合并，u64最大值无损发送", async () => {
    const { first, sent, dispatcher } = blockedSender();
    const contexts = [
      { streamId: "9007199254740992", inputEpoch: "9007199254740992" },
      { streamId: "9007199254740992", inputEpoch: "9007199254740993" },
      { streamId: "9007199254740993", inputEpoch: "9007199254740993" },
      { streamId: "18446744073709551615", inputEpoch: "18446744073709551615" },
    ];
    dispatcher.enqueue(blockingKey, oldContext);
    for (const context of contexts) dispatcher.enqueue({ kind: "wheel", deltaX: 10, deltaY: 20 }, context);
    first.resolve();
    await dispatcher.drain();
    expect(sent.slice(1).map(({ context }) => context)).toEqual(contexts);
    expect(sent.slice(1).map(({ input }) => input)).toEqual(contexts.map(() => ({ kind: "wheel", deltaX: 10, deltaY: 20 })));
  });

  test("u64最大身份的文字分片和快捷键down/up不发生number转换", async () => {
    const context = { streamId: "18446744073709551615", inputEpoch: "9007199254740993" };
    const observed: Array<{ kind: string; context: RemoteInputContext }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, pair) => {
      observed.push({ kind: `${input.kind}:${input.pressed}`, context: { ...pair } });
    }, undefined, async (text, pair) => {
      observed.push({ kind: `text:${Array.from(text).length}`, context: { ...pair } });
    });
    const text = dispatcher.enqueueText("😀".repeat(257), context);
    const shortcut = dispatcher.enqueueShortcut({ kind: "key", key: "escape", pressed: true }, context);
    await text;
    await shortcut;
    await dispatcher.drain();
    expect(observed).toEqual([
      { kind: "text:256", context }, { kind: "text:1", context },
      { kind: "key:true", context }, { kind: "key:false", context },
    ]);
  });

  test("同stream不同epoch的滚轮距离分别发送，不能合并", async () => {
    const { first, sent, dispatcher } = blockedSender();
    dispatcher.enqueue(blockingKey, oldContext);
    dispatcher.enqueue({ kind: "wheel", deltaX: 10, deltaY: 100 }, oldContext);
    dispatcher.enqueue({ kind: "wheel", deltaX: 20, deltaY: 200 }, newContext);
    first.resolve();
    await dispatcher.drain();
    expect(sent.slice(1)).toEqual([
      { input: { kind: "wheel", deltaX: 10, deltaY: 100 }, context: oldContext },
      { input: { kind: "wheel", deltaX: 20, deltaY: 200 }, context: newContext },
    ]);
  });

  test("同epoch指针可合并，不同epoch指针不得互相覆盖", async () => {
    const { first, sent, dispatcher } = blockedSender();
    dispatcher.enqueue(blockingKey, oldContext);
    dispatcher.enqueue({ kind: "mouseMove", x: 1, y: 1 }, oldContext);
    dispatcher.enqueue({ kind: "mouseMove", x: 2, y: 2 }, oldContext);
    dispatcher.enqueue({ kind: "mouseMove", x: 3, y: 3 }, newContext);
    dispatcher.enqueue({ kind: "mouseMove", x: 4, y: 4 }, newContext);
    first.resolve();
    await dispatcher.drain();
    expect(sent.slice(1)).toEqual([
      { input: { kind: "mouseMove", x: 2, y: 2 }, context: oldContext },
      { input: { kind: "mouseMove", x: 4, y: 4 }, context: newContext },
    ]);
  });

  test("独立待发送指针复用槽保存primitive快照，原context修改不重标", async () => {
    const { first, sent, dispatcher } = blockedSender();
    const mutable = { ...oldContext };
    dispatcher.enqueue(blockingKey, oldContext);
    dispatcher.enqueueMouseMove(10, 20, mutable);
    mutable.streamId = "99";
    mutable.inputEpoch = "99";
    dispatcher.enqueueMouseMove(30, 40, newContext);
    first.resolve();
    await dispatcher.drain();
    expect(sent.slice(1)).toEqual([
      { input: { kind: "mouseMove", x: 10, y: 20 }, context: oldContext },
      { input: { kind: "mouseMove", x: 30, y: 40 }, context: newContext },
    ]);
  });

  test("离散输入与快捷键入队后修改原context也不会重标", async () => {
    const { first, sent, dispatcher } = blockedSender();
    const mutable = { ...oldContext };
    dispatcher.enqueue(blockingKey, oldContext);
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, mutable);
    const shortcut = dispatcher.enqueueShortcut({ kind: "key", key: "escape", pressed: true, modifiers: 3 }, mutable);
    mutable.streamId = "99";
    mutable.inputEpoch = "99";
    first.resolve();
    await shortcut;
    await dispatcher.drain();
    expect(sent.slice(1).map(({ context }) => context)).toEqual([oldContext, oldContext, oldContext]);
    expect(sent.slice(1).map(({ input }) => [input.key ?? input.button, input.pressed])).toEqual([
      ["left", true], ["escape", true], ["escape", false],
    ]);
  });

  test("Unicode分片与后续鼠标按键共享各自产生时context并保持顺序", async () => {
    const first = deferred();
    const observed: Array<{ value: string; context: RemoteInputContext }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, context) => {
      observed.push({ value: `${input.kind}:${input.key ?? input.button}:${input.pressed}`, context: { ...context } });
      if (observed.length === 1) await first.promise;
    }, undefined, async (text, context) => {
      observed.push({ value: `text:${Array.from(text).length}`, context: { ...context } });
    });
    const mutable = { ...oldContext };
    dispatcher.enqueue(blockingKey, oldContext);
    const text = dispatcher.enqueueText("😀".repeat(300), mutable);
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, mutable);
    dispatcher.enqueue({ kind: "key", key: "shift", pressed: false, modifiers: 0 }, mutable);
    mutable.inputEpoch = newContext.inputEpoch;
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: true, modifiers: 0 }, mutable);
    first.resolve();
    await text;
    await dispatcher.drain();
    expect(observed).toEqual([
      { value: "key:shift:true", context: oldContext },
      { value: "text:256", context: oldContext },
      { value: "text:44", context: oldContext },
      { value: "mouseButton:left:true", context: oldContext },
      { value: "key:shift:false", context: oldContext },
      { value: "key:enter:true", context: newContext },
    ]);
  });

  test("取消正在发送的快捷键只用旧pair清理，随后新epoch按键保持新pair", async () => {
    const { first, sent, dispatcher } = blockedSender();
    const down: ControllerInput = { kind: "key", key: "tab", pressed: true, modifiers: 4 };
    const shortcut = dispatcher.enqueueShortcut(down, oldContext);
    const cancelled = shortcut.catch((error: Error) => error);
    dispatcher.discardAll();
    dispatcher.enqueue(down, newContext);
    first.resolve();
    expect(await cancelled).toBeInstanceOf(Error);
    await dispatcher.drain();
    expect(sent).toEqual([
      { input: down, context: oldContext },
      { input: { ...down, pressed: false }, context: oldContext },
      { input: down, context: newContext },
    ]);
  });

  test("5000单位滚轮分片在背压与原对象修改后仍保持原pair和总量", async () => {
    const { first, sent, dispatcher } = blockedSender();
    const mutable = { ...oldContext };
    dispatcher.enqueue({ kind: "wheel", deltaX: 5000, deltaY: -5000 }, mutable);
    mutable.streamId = "99";
    mutable.inputEpoch = "99";
    dispatcher.enqueue({ kind: "wheel", deltaX: -100, deltaY: 100 }, newContext);
    first.resolve();
    await dispatcher.drain();
    const original = sent.slice(0, 5);
    expect(original.map(({ context }) => context)).toEqual(Array.from({ length: 5 }, () => oldContext));
    expect(original.reduce((sum, { input }) => sum + (input.deltaX ?? 0), 0)).toBe(5000);
    expect(original.reduce((sum, { input }) => sum + (input.deltaY ?? 0), 0)).toBe(-5000);
    expect(original.every(({ input }) => Math.abs(input.deltaX ?? 0) <= 1200 && Math.abs(input.deltaY ?? 0) <= 1200)).toBe(true);
    expect(sent[5]).toEqual({ input: { kind: "wheel", deltaX: -100, deltaY: 100 }, context: newContext });
  });

  test("epoch替换清除排队旧文字，新文字不继承旧epoch", async () => {
    const first = deferred();
    const observed: Array<{ text: string; context: RemoteInputContext }> = [];
    const dispatcher = new RemoteInputDispatcher(async () => { await first.promise; }, undefined, async (text, context) => {
      observed.push({ text, context: { ...context } });
    });
    dispatcher.enqueue(blockingKey, oldContext);
    const old = dispatcher.enqueueText("旧文字", oldContext).catch((error: Error) => error);
    dispatcher.discardAll();
    const current = dispatcher.enqueueText("新文字", newContext);
    first.resolve();
    expect(await old).toBeInstanceOf(Error);
    await current;
    await dispatcher.drain();
    expect(observed).toEqual([{ text: "新文字", context: newContext }]);
  });

  test.each([
    { streamId: "41", inputEpoch: "0" },
    { streamId: "0", inputEpoch: "7" },
    { streamId: 41, inputEpoch: "7" },
    { streamId: "41", inputEpoch: 7 },
    { streamId: "041", inputEpoch: "7" },
    { streamId: "41", inputEpoch: "+7" },
    { streamId: "41", inputEpoch: "7.0" },
    { streamId: "41", inputEpoch: "7\n" },
    { streamId: "18446744073709551616", inputEpoch: "7" },
    { streamId: "41", inputEpoch: "18446744073709551616" },
    { streamId: "41" },
    { inputEpoch: "7" },
    null,
    undefined,
  ])("无效或缺失context %#不能发送或补齐", async (malformed) => {
    const sent: unknown[] = [];
    let errors = 0;
    const dispatcher = new RemoteInputDispatcher(async (input, context) => { sent.push([input, context]); }, () => { errors++; }, async (text, context) => { sent.push([text, context]); });
    const context = malformed as RemoteInputContext;
    dispatcher.enqueue(blockingKey, context);
    dispatcher.enqueueMouseMove(1, 2, context);
    await expect(dispatcher.enqueueText("不可发送", context)).rejects.toThrow("Invalid remote input context");
    await expect(dispatcher.enqueueShortcut({ kind: "key", key: "escape", pressed: true }, context)).rejects.toThrow("Invalid remote input context");
    await dispatcher.drain();
    expect(errors).toBe(2);
    expect(sent).toEqual([]);
  });
});
