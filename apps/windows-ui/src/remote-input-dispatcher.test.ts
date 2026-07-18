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
    const sent: ControllerInput[] = [];
    let calls = 0;
    const dispatcher = new RemoteInputDispatcher(async (input) => {
      sent.push(input);
      calls += 1;
      if (calls === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 10, y: 10 });
    dispatcher.enqueue({ kind: "mouseMove", x: 20, y: 20 });
    dispatcher.enqueue({ kind: "mouseMove", x: 30, y: 30 });

    expect(sent).toEqual([{ kind: "mouseMove", x: 10, y: 10 }]);
    first.resolve();
    await dispatcher.drain();
    expect(sent).toEqual([
      { kind: "mouseMove", x: 10, y: 10 },
      { kind: "mouseMove", x: 30, y: 30 },
    ]);
  });

  test("preserves button ordering around coalesced movement", async () => {
    const sent: ControllerInput[] = [];
    const dispatcher = new RemoteInputDispatcher(async (input) => {
      sent.push(input);
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 10, y: 10 });
    dispatcher.enqueue({ kind: "mouseMove", x: 20, y: 20 });
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: true });
    dispatcher.enqueue({ kind: "mouseMove", x: 30, y: 30 });
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: false });

    await dispatcher.drain();
    expect(sent).toEqual([
      { kind: "mouseMove", x: 10, y: 10 },
      { kind: "mouseMove", x: 20, y: 20 },
      { kind: "mouseButton", button: "left", pressed: true },
      { kind: "mouseMove", x: 30, y: 30 },
      { kind: "mouseButton", button: "left", pressed: false },
    ]);
  });

  test("can discard stale movement without dropping a release event", async () => {
    const first = deferred();
    const sent: ControllerInput[] = [];
    const dispatcher = new RemoteInputDispatcher(async (input) => {
      sent.push(input);
      if (sent.length === 1) {
        await first.promise;
      }
    });

    dispatcher.enqueue({ kind: "mouseMove", x: 1, y: 1 });
    dispatcher.enqueue({ kind: "mouseMove", x: 2, y: 2 });
    dispatcher.enqueue({ kind: "mouseButton", button: "left", pressed: false });
    dispatcher.discardPendingMoves();
    first.resolve();
    await dispatcher.drain();

    expect(sent).toEqual([
      { kind: "mouseMove", x: 1, y: 1 },
      { kind: "mouseButton", button: "left", pressed: false },
    ]);
  });
});
