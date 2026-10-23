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
    }, undefined, async (value, streamId) => {
      sent.push(`text:${value}:${streamId}`);
      await text.promise;
    });
    dispatcher.enqueue({ kind: "key", key: "shift", pressed: true }, 7);
    dispatcher.enqueue({ kind: "key", key: "shift", pressed: false }, 7);
    const committed = dispatcher.enqueueText("中文", 7);
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: true }, 7);
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: false }, 7);
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, 7);
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
    await dispatcher.enqueueText("😀".repeat(300), 8);
    expect(sent.map((text) => Array.from(text).length)).toEqual([256, 44]);
    expect(sent.join("")).toBe("😀".repeat(300));
    expect(new TextEncoder().encode(sent[0]).length).toBe(1024);
  });

  test("disconnect rejects queued text and never sends it to a later stream", async () => {
    const first = deferred();
    const texts: string[] = [];
    const dispatcher = new RemoteInputDispatcher(async () => { await first.promise; }, undefined, async (text) => { texts.push(text); });
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: true }, 9);
    const pending = dispatcher.enqueueText("旧会话", 9);
    const rejected = pending.catch((error: Error) => error.message);
    dispatcher.discardAll();
    first.resolve();
    expect(await rejected).toBe("Remote input was cleared");
    await dispatcher.drain();
    expect(texts).toEqual([]);
    await dispatcher.enqueueText("新会话", 10);
    expect(texts).toEqual(["新会话"]);
  });

  test("keeps one IPC call in flight and collapses consecutive pointer moves", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: number }> = [];
    let calls = 0;
    const dispatcher = new RemoteInputDispatcher(async (input, streamId) => {
      sent.push({ input, streamId });
      calls += 1;
      if (calls === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 10, y: 10 }, 7);
    dispatcher.enqueue({ kind: "mouseMove", x: 20, y: 20 }, 7);
    dispatcher.enqueue({ kind: "mouseMove", x: 30, y: 30 }, 7);

    expect(sent).toEqual([{ input: { kind: "mouseMove", x: 10, y: 10 }, streamId: 7 }]);
    first.resolve();
    await dispatcher.drain();
    expect(sent).toEqual([
      { input: { kind: "mouseMove", x: 10, y: 10 }, streamId: 7 },
      { input: { kind: "mouseMove", x: 30, y: 30 }, streamId: 7 },
    ]);
  });

  test("reuses a pending pointer move while IPC is busy", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: number }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, streamId) => {
      sent.push({ input, streamId });
      if (sent.length === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "key", key: "a", pressed: true }, 7);
    await Promise.resolve();
    dispatcher.enqueueMouseMove(10, 10, 7);
    dispatcher.enqueueMouseMove(20, 20, 7);
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual([
      { input: { kind: "key", key: "a", pressed: true }, streamId: 7 },
      { input: { kind: "mouseMove", x: 20, y: 20 }, streamId: 7 },
    ]);
  });

  test("flushes a pending pointer move before a discrete input", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: number }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, streamId) => {
      sent.push({ input, streamId });
      if (sent.length === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "key", key: "a", pressed: true }, 7);
    await Promise.resolve();
    dispatcher.enqueueMouseMove(30, 40, 7);
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, 7);
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual([
      { input: { kind: "key", key: "a", pressed: true }, streamId: 7 },
      { input: { kind: "mouseMove", x: 30, y: 40 }, streamId: 7 },
      { input: { kind: "mouseButton", button: "left", pressed: true }, streamId: 7 },
    ]);
  });

  test("coalesces a stalled wheel burst into one bounded event", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: number }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, streamId) => {
      sent.push({ input, streamId });
      if (sent.length === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "key", key: "a", pressed: true }, 7);
    for (let index = 0; index < 10_000; index += 1) {
      dispatcher.enqueue({ kind: "wheel", deltaX: 1, deltaY: 1 }, 7);
    }
    first.resolve();
    await dispatcher.drain();

    expect(sent).toHaveLength(2);
    expect(sent[1]).toEqual({
      input: { kind: "wheel", deltaX: 4_800, deltaY: 4_800 },
      streamId: 7,
    });
  });

  test("preserves button ordering around coalesced movement", async () => {
    const sent: Array<{ input: ControllerInput; streamId: number }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, streamId) => {
      sent.push({ input, streamId });
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 10, y: 10 }, 9);
    dispatcher.enqueue({ kind: "mouseMove", x: 20, y: 20 }, 9);
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true }, 9);
    dispatcher.enqueue({ kind: "mouseMove", x: 30, y: 30 }, 9);
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: false }, 9);

    await dispatcher.drain();
    expect(sent).toEqual([
      { input: { kind: "mouseMove", x: 10, y: 10 }, streamId: 9 },
      { input: { kind: "mouseMove", x: 20, y: 20 }, streamId: 9 },
      { input: { kind: "mouseButton", button: "left", pressed: true }, streamId: 9 },
      { input: { kind: "mouseMove", x: 30, y: 30 }, streamId: 9 },
      { input: { kind: "mouseButton", button: "left", pressed: false }, streamId: 9 },
    ]);
  });

  test("can discard stale movement without dropping a release event", async () => {
    const first = deferred();
    const sent: Array<{ input: ControllerInput; streamId: number }> = [];
    const dispatcher = new RemoteInputDispatcher(async (input, streamId) => {
      sent.push({ input, streamId });
      if (sent.length === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 1, y: 1 }, 11);
    dispatcher.enqueue({ kind: "mouseMove", x: 2, y: 2 }, 11);
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: false }, 11);
    dispatcher.discardPendingMoves();
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual([
      { input: { kind: "mouseMove", x: 1, y: 1 }, streamId: 11 },
      { input: { kind: "mouseButton", button: "left", pressed: false }, streamId: 11 },
    ]);
  });

  test("never coalesces movement across stream generations", async () => {
    const first = deferred();
    const sent: number[] = [];
    const dispatcher = new RemoteInputDispatcher(async (_input, streamId) => {
      sent.push(streamId);
      if (sent.length === 1) await first.promise;
    });

    dispatcher.enqueueMouseMove(1, 1, 20);
    dispatcher.enqueueMouseMove(2, 2, 20);
    dispatcher.enqueueMouseMove(3, 3, 21);
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual([20, 20, 21]);
  });

  test("can clear every queued event when a session disconnects", async () => {
    const first = deferred();
    const sent: number[] = [];
    const dispatcher = new RemoteInputDispatcher(async (_input, streamId) => {
      sent.push(streamId);
      if (sent.length === 1) await first.promise;
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 1, y: 1 }, 30);
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: true }, 30);
    dispatcher.enqueue({ kind: "key", key: "enter", pressed: false }, 30);
    dispatcher.discardAll();
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual([30]);
  });
});
