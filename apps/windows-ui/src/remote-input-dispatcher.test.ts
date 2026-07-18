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

    dispatcher.enqueue({ kind: "mouseMove", x: 1, y: 1 }, 20);
    dispatcher.enqueue({ kind: "mouseMove", x: 2, y: 2 }, 20);
    dispatcher.enqueue({ kind: "mouseMove", x: 3, y: 3 }, 21);
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
