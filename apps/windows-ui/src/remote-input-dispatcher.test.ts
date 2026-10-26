import { describe, expect, test } from "bun:test";

import { RemoteInputDispatcher } from "./remote-input-dispatcher";
import type { ControllerInput } from "./types";

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve = () => {};
  const promise = new Promise<void>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe("remote input dispatcher", () => {
  test("Unicode commits and subsequent Enter/mouse stay in the same ordered IPC queue", async () => {
    const first = deferred();
    const text = deferred();
    const sent: string[] = [];
    const dispatcher = new RemoteInputDispatcher(async (input) => {
      sent.push(`${input.kind}:${input.key ?? input.button}:${input.pressed}`);
      if (sent.length === 1) await first.promise;
    }, undefined, async (value, { streamId }) => {
      sent.push(`text:${value}:${streamId}`);
      await text.promise;
    });
    dispatcher.enqueue({ kind: "key", key: "shift", pressed: true }, { streamId: "7", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "key", key: "shift", pressed: false }, { streamId: "7", inputEpoch: "1" });
    const committed = dispatcher.enqueueText("中文", { streamId: "7", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: true }, { streamId: "7", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: false }, { streamId: "7", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, { streamId: "7", inputEpoch: "1" });
    first.resolve();
    for (let index = 0; index < 8; index += 1) await Promise.resolve();
    expect(sent).toEqual(["key:shift:true", "key:shift:false", "text:中文:7"]);
    text.resolve();
    await committed;
    await dispatcher.drain();
    expect(sent).toEqual(["key:shift:true", "key:shift:false", "text:中文:7", "key:enter:true", "key:enter:false", "mouseButton:left:true"]);
  });

  test("long Unicode text is split at scalar boundaries within the native command limit", async () => {
    const sent: string[] = [];
    const dispatcher = new RemoteInputDispatcher(async () => {}, undefined, async (text) => { sent.push(text); });
    await dispatcher.enqueueText("😀".repeat(300), { streamId: "8", inputEpoch: "1" });
    expect(sent.map((text) => Array.from(text).length)).toEqual([256, 44]);
    expect(sent.join("")).toBe("😀".repeat(300));
    expect(new TextEncoder().encode(sent[0]).length).toBe(1024);
  });

  test("disconnect rejects queued text and never sends it to a later stream", async () => {
    const first = deferred();
    const texts: string[] = [];
    const dispatcher = new RemoteInputDispatcher(async () => { await first.promise; }, undefined, async (text) => { texts.push(text); });
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: true }, { streamId: "9", inputEpoch: "1" });
    const pending = dispatcher.enqueueText("旧会话", { streamId: "9", inputEpoch: "1" });
    const rejected = pending.catch((error: Error) => error.message);
    dispatcher.discardAll();
    first.resolve();
    expect(await rejected).toBe("Remote input was cleared");
    await dispatcher.drain();
    expect(texts).toEqual([]);
    await dispatcher.enqueueText("新会话", { streamId: "10", inputEpoch: "1" });
    expect(texts).toEqual(["新会话"]);
  });

  test("keeps one IPC call in flight and collapses consecutive pointer moves", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: string }> = [];
    let calls = 0;
    const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => {
      sent.push({ input, streamId });
      calls += 1;
      if (calls === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 10, y: 10 }, { streamId: "7", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseMove", x: 20, y: 20 }, { streamId: "7", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseMove", x: 30, y: 30 }, { streamId: "7", inputEpoch: "1" });

    expect(sent).toEqual([{ input: { kind: "mouseMove", x: 10, y: 10 }, streamId: "7" }]);
    first.resolve();
    await dispatcher.drain();
    expect(sent).toEqual([
      { input: { kind: "mouseMove", x: 10, y: 10 }, streamId: "7" },
      { input: { kind: "mouseMove", x: 30, y: 30 }, streamId: "7" },
    ]);
  });

  test("reuses a pending pointer move while IPC is busy", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: string }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => {
      sent.push({ input, streamId });
      if (sent.length === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "key", key: "a", pressed: true }, { streamId: "7", inputEpoch: "1" });
    await Promise.resolve();
    dispatcher.enqueueMouseMove(10, 10, { streamId: "7", inputEpoch: "1" });
    dispatcher.enqueueMouseMove(20, 20, { streamId: "7", inputEpoch: "1" });
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual([
      { input: { kind: "key", key: "a", pressed: true }, streamId: "7" },
      { input: { kind: "mouseMove", x: 20, y: 20 }, streamId: "7" },
    ]);
  });

  test("flushes a pending pointer move before a discrete input", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: string }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => {
      sent.push({ input, streamId });
      if (sent.length === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "key", key: "a", pressed: true }, { streamId: "7", inputEpoch: "1" });
    await Promise.resolve();
    dispatcher.enqueueMouseMove(30, 40, { streamId: "7", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, { streamId: "7", inputEpoch: "1" });
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual([
      { input: { kind: "key", key: "a", pressed: true }, streamId: "7" },
      { input: { kind: "mouseMove", x: 30, y: 40 }, streamId: "7" },
      { input: { kind: "mouseButton", button: "left", pressed: true }, streamId: "7" },
    ]);
  });

  test("splits a stalled wheel burst within the native bound without losing distance", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: string }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => {
      sent.push({ input, streamId });
      if (sent.length === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "key", key: "a", pressed: true }, { streamId: "7", inputEpoch: "1" });
    for (let index = 0; index < 10_000; index += 1) {
      dispatcher.enqueue({ kind: "wheel", deltaX: 1, deltaY: 1 }, { streamId: "7", inputEpoch: "1" });
    }
    first.resolve();
    await dispatcher.drain();

    expect(sent).toHaveLength(10);
    const wheels = sent.slice(1).map(({ input }) => input);
    expect(wheels.every(({ deltaX = 0, deltaY = 0 }) => Math.abs(deltaX) <= 1200 && Math.abs(deltaY) <= 1200)).toBe(true);
    expect(wheels.reduce((sum, input) => sum + (input.deltaX ?? 0), 0)).toBe(10_000);
    expect(wheels.reduce((sum, input) => sum + (input.deltaY ?? 0), 0)).toBe(10_000);
  });

  test("preserves button ordering around coalesced movement", async () => {
    const sent: Array<{ input: ControllerInput; streamId: string }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => {
      sent.push({ input, streamId });
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 10, y: 10 }, { streamId: "9", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseMove", x: 20, y: 20 }, { streamId: "9", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, { streamId: "9", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseMove", x: 30, y: 30 }, { streamId: "9", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: false }, { streamId: "9", inputEpoch: "1" });

    await dispatcher.drain();
    expect(sent).toEqual([
      { input: { kind: "mouseMove", x: 10, y: 10 }, streamId: "9" },
      { input: { kind: "mouseMove", x: 20, y: 20 }, streamId: "9" },
      { input: { kind: "mouseButton", button: "left", pressed: true }, streamId: "9" },
      { input: { kind: "mouseMove", x: 30, y: 30 }, streamId: "9" },
      { input: { kind: "mouseButton", button: "left", pressed: false }, streamId: "9" },
    ]);
  });

  test("can discard stale movement without dropping a release event", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: string }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => {
      sent.push({ input, streamId });
      if (sent.length === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 1, y: 1 }, { streamId: "11", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseMove", x: 2, y: 2 }, { streamId: "11", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: false }, { streamId: "11", inputEpoch: "1" });
    dispatcher.discardPendingMoves();
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual([
      { input: { kind: "mouseMove", x: 1, y: 1 }, streamId: "11" },
      { input: { kind: "mouseButton", button: "left", pressed: false }, streamId: "11" },
    ]);
  });

  test("never coalesces movement across stream generations", async () => {
    const first = deferred();
    const sent: string[] = [];
    const dispatcher = new RemoteInputDispatcher(async (_input, { streamId }) => {
      sent.push(streamId);
      if (sent.length === 1) await first.promise;
    });

    dispatcher.enqueueMouseMove(1, 1, { streamId: "20", inputEpoch: "1" });
    dispatcher.enqueueMouseMove(2, 2, { streamId: "20", inputEpoch: "1" });
    dispatcher.enqueueMouseMove(3, 3, { streamId: "21", inputEpoch: "1" });
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual(["20", "20", "21"]);
  });

  test("can clear every queued event when a session disconnects", async () => {
    const first = deferred();
    const sent: string[] = [];
    const dispatcher = new RemoteInputDispatcher(async (_input, { streamId }) => {
      sent.push(streamId);
      if (sent.length === 1) await first.promise;
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 1, y: 1 }, { streamId: "30", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: true }, { streamId: "30", inputEpoch: "1" });
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: false }, { streamId: "30", inputEpoch: "1" });
    dispatcher.discardAll();
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual(["30"]);
  });
});

test("5000 units preserve horizontal direction reversals and button ordering", async () => {
  const first = deferred();
  const sent: Array<{ input: ControllerInput; streamId: string }> = [];
  const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => {
    sent.push({ input, streamId });
    if (sent.length === 1) await first.promise;
  });
  dispatcher.enqueue({ kind: "key", key: "a", pressed: true }, { streamId: "1", inputEpoch: "1" });
  for (let n = 0; n < 50; n++) dispatcher.enqueue({ kind: "wheel", deltaX: 100, deltaY: -100 }, { streamId: "1", inputEpoch: "1" });
  dispatcher.enqueue({ kind: "wheel", deltaX: -500, deltaY: 500 }, { streamId: "1", inputEpoch: "1" });
  dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, { streamId: "1", inputEpoch: "1" });
  dispatcher.enqueue({ kind: "wheel", deltaX: 200, deltaY: 200 }, { streamId: "2", inputEpoch: "1" });
  first.resolve();
  await dispatcher.drain();
  expect(sent.slice(1, 6).map(({input}) => input.deltaX)).toEqual([1200,1200,1200,1200,200]);
  expect(sent.slice(1, 6).reduce((sum, {input}) => sum + (input.deltaY ?? 0), 0)).toBe(-5000);
  expect(sent.slice(6)).toEqual([
    { input: {kind: "wheel", deltaX: -500, deltaY: 500}, streamId: "1" },
    { input: {kind: "mouseButton", button: "left", pressed: true}, streamId: "1" },
    { input: {kind: "wheel", deltaX: 200, deltaY: 200}, streamId: "2" },
  ]);
});

test("discard cancels remaining fragments of a wheel already being sent", async () => {
  const first = deferred();
  const sent: Array<{ input: ControllerInput; streamId: string }> = [];
  const dispatcher = new RemoteInputDispatcher(async (input, { streamId }) => {
    sent.push({ input, streamId });
    if (sent.length === 1) await first.promise;
  });
  dispatcher.enqueue({kind: "wheel", deltaY: 5000}, { streamId: "1", inputEpoch: "1" });
  dispatcher.discardAll();
  dispatcher.enqueue({kind: "wheel", deltaY: -100}, { streamId: "2", inputEpoch: "1" });
  first.resolve();
  await dispatcher.drain();
  expect(sent.map(({input,streamId}) => [input.deltaY, streamId])).toEqual([[1200,"1"],[-100,"2"]]);
});

test("invalid wheel values cannot create a non-terminating fragment loop", async () => {
  let errors = 0;
  const sent: ControllerInput[] = [];
  const dispatcher = new RemoteInputDispatcher(async (input) => { sent.push(input); }, () => { errors++; });
  for (const deltaY of [Infinity, NaN, 1.5]) dispatcher.enqueue({ kind: "wheel", deltaY }, { streamId: "1", inputEpoch: "1" });
  await dispatcher.drain();
  expect(errors).toBe(3);
  expect(sent).toEqual([]);
});
