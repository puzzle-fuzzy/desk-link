import { describe, expect, test } from "bun:test";

import {
  SerialVideoPull,
  nextVideoPullFailureCount,
  type VideoPullKey,
  videoPullRetryDelay,
} from "./video-pull-loop";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((next, fail) => {
    resolve = next;
    reject = fail;
  });
  return { promise, resolve, reject };
}

const firstKey: VideoPullKey = { streamId: 9, configVersion: 3 };

function manualWait() {
  const pending: Array<{ delay: number; release: () => void }> = [];
  return {
    pending,
    wait: (delay: number) => new Promise<void>((resolve) => {
      pending.push({ delay, release: resolve });
    }),
  };
}

describe("serial video pull", () => {
  test("keeps cumulative diagnostic failures inside the Rust boundary", () => {
    expect(nextVideoPullFailureCount(0)).toBe(1);
    expect(nextVideoPullFailureCount(999_999)).toBe(1_000_000);
    expect(nextVideoPullFailureCount(1_000_000)).toBe(1_000_000);
  });

  test("uses capped retry delays", () => {
    expect([1, 2, 3, 4, 5, 6].map(videoPullRetryDelay)).toEqual([
      100,
      250,
      500,
      1_000,
      2_000,
      2_000,
    ]);
  });

  test("retries a pull failure without creating a second in-flight request", async () => {
    const first = deferred<number>();
    const second = deferred<number>();
    const requests = [first, second];
    const retryWait = manualWait();
    const failures: Array<{ attempt: number; delay: number }> = [];
    const delivered: number[] = [];
    let calls = 0;
    const pull = new SerialVideoPull<number>(retryWait.wait);
    pull.start(
      firstKey,
      () => requests[calls++]!.promise,
      (value) => {
        delivered.push(value);
        pull.stop();
      },
      undefined,
      (_error, attempt, delay) => failures.push({ attempt, delay }),
    );
    first.reject(new Error("temporary IPC failure"));
    await Promise.resolve();
    await Promise.resolve();

    expect(calls).toBe(1);
    expect(retryWait.pending.map((item) => item.delay)).toEqual([100]);
    expect(failures).toEqual([{ attempt: 1, delay: 100 }]);

    retryWait.pending.shift()!.release();
    await Promise.resolve();
    await Promise.resolve();
    expect(calls).toBe(2);

    second.resolve(20);
    await Promise.resolve();
    await Promise.resolve();
    expect(delivered).toEqual([20]);
  });

  test("a successful frame resets the consecutive failure delay", async () => {
    const first = deferred<number>();
    const second = deferred<number>();
    const third = deferred<number>();
    const requests = [first, second, third];
    const retryWait = manualWait();
    const failures: Array<{ attempt: number; delay: number }> = [];
    let calls = 0;
    const pull = new SerialVideoPull<number>(retryWait.wait);
    pull.start(
      firstKey,
      () => requests[calls++]!.promise,
      () => {},
      undefined,
      (_error, attempt, delay) => failures.push({ attempt, delay }),
    );

    first.reject(new Error("first failure"));
    await Promise.resolve();
    await Promise.resolve();
    retryWait.pending.shift()!.release();
    await Promise.resolve();
    await Promise.resolve();
    second.resolve(20);
    await Promise.resolve();
    await Promise.resolve();
    third.reject(new Error("failure after success"));
    await Promise.resolve();
    await Promise.resolve();

    expect(failures).toEqual([
      { attempt: 1, delay: 100 },
      { attempt: 1, delay: 100 },
    ]);
    pull.stop();
    retryWait.pending.shift()!.release();
  });

  test("stop during retry wait prevents another backend request", async () => {
    const first = deferred<number>();
    const retryWait = manualWait();
    let calls = 0;
    const pull = new SerialVideoPull<number>(retryWait.wait);
    pull.start(firstKey, () => {
      calls += 1;
      return first.promise;
    }, () => {});
    first.reject(new Error("closed"));
    await Promise.resolve();
    await Promise.resolve();

    pull.stop();
    retryWait.pending.shift()!.release();
    await Promise.resolve();
    await Promise.resolve();

    expect(calls).toBe(1);
  });

  test("keeps exactly one backend request in flight", async () => {
    const first = deferred<number>();
    const second = deferred<number>();
    const requests = [first, second];
    let calls = 0;
    const delivered: number[] = [];
    const pull = new SerialVideoPull<number>();

    pull.start(firstKey, () => requests[calls++]!.promise, (value) => delivered.push(value));
    await Promise.resolve();
    expect(calls).toBe(1);

    first.resolve(10);
    await Promise.resolve();
    await Promise.resolve();
    expect(delivered).toEqual([10]);
    expect(calls).toBe(2);

    pull.stop();
    second.reject(new Error("closed"));
  });

  test("starting the same stream configuration is idempotent", async () => {
    const pending = deferred<number>();
    let calls = 0;
    const pull = new SerialVideoPull<number>();
    const request = () => {
      calls += 1;
      return pending.promise;
    };

    pull.start(firstKey, request, () => {});
    pull.start({ ...firstKey }, request, () => {});
    await Promise.resolve();

    expect(calls).toBe(1);
    pull.stop();
    pending.reject(new Error("closed"));
  });

  test("stop discards a late response", async () => {
    const pending = deferred<number>();
    const delivered: number[] = [];
    const pull = new SerialVideoPull<number>();
    pull.start(firstKey, () => pending.promise, (value) => delivered.push(value));
    await Promise.resolve();

    pull.stop();
    pending.resolve(10);
    await Promise.resolve();
    await Promise.resolve();

    expect(delivered).toEqual([]);
  });

  test("a new configuration invalidates the old response", async () => {
    const oldFrame = deferred<number>();
    const newFrame = deferred<number>();
    const delivered: number[] = [];
    const pull = new SerialVideoPull<number>();
    pull.start(firstKey, () => oldFrame.promise, (value) => delivered.push(value));
    await Promise.resolve();

    pull.start(
      { streamId: 9, configVersion: 4 },
      () => newFrame.promise,
      (value) => {
        delivered.push(value);
        pull.stop();
      },
    );
    await Promise.resolve();
    oldFrame.resolve(10);
    newFrame.resolve(20);
    await Promise.resolve();
    await Promise.resolve();

    expect(delivered).toEqual([20]);
    pull.stop();
  });

  test("a delivery error is reported without stopping the pull loop", async () => {
    const first = deferred<number>();
    const second = deferred<number>();
    const third = deferred<number>();
    const requests = [first, second, third];
    const errors: unknown[] = [];
    let calls = 0;
    const pull = new SerialVideoPull<number>();
    pull.start(
      firstKey,
      () => requests[calls++]!.promise,
      (value) => {
        if (value === 10) {
          throw new Error("malformed");
        }
      },
      (error) => errors.push(error),
    );

    first.resolve(10);
    await Promise.resolve();
    await Promise.resolve();
    expect(errors).toHaveLength(1);
    expect(calls).toBe(2);

    second.resolve(20);
    await Promise.resolve();
    await Promise.resolve();
    expect(calls).toBe(3);
    pull.stop();
    third.reject(new Error("closed"));
  });
});
