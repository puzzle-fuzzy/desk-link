import { describe, expect, test } from "bun:test";

import { LatestFrameScheduler, RenderScheduler } from "./render-scheduler";

describe("render scheduler", () => {
  test("coalesces a burst into one animation frame", () => {
    const callbacks: Array<() => void> = [];
    const scheduler = new RenderScheduler((callback) => {
      callbacks.push(callback);
      return callbacks.length;
    });
    let renders = 0;

    scheduler.schedule(() => { renders += 1; });
    scheduler.schedule(() => { renders += 1; });
    scheduler.schedule(() => { renders += 1; });

    expect(callbacks).toHaveLength(1);
    expect(scheduler.pending).toBeTrue();
    callbacks[0]!();
    expect(renders).toBe(1);
    expect(scheduler.pending).toBeFalse();
  });

  test("allows the next frame after the first render commits", () => {
    const callbacks: Array<() => void> = [];
    const scheduler = new RenderScheduler((callback) => {
      callbacks.push(callback);
      return callbacks.length;
    });
    let renders = 0;

    scheduler.schedule(() => { renders += 1; });
    callbacks[0]!();
    scheduler.schedule(() => { renders += 1; });
    callbacks[1]!();

    expect(renders).toBe(2);
    expect(callbacks).toHaveLength(2);
  });

  test("cancels a pending frame without running the render", () => {
    const callbacks: Array<() => void> = [];
    const cancelled: number[] = [];
    const scheduler = new RenderScheduler(
      (callback) => {
        callbacks.push(callback);
        return callbacks.length;
      },
      (handle) => { cancelled.push(handle); },
    );
    let renders = 0;

    scheduler.schedule(() => { renders += 1; });
    scheduler.cancel();
    callbacks[0]!();

    expect(cancelled).toEqual([1]);
    expect(renders).toBe(0);
    expect(scheduler.pending).toBeFalse();
  });

  test("commits only the latest value from a burst", () => {
    const callbacks: Array<() => void> = [];
    const committed: number[] = [];
    const scheduler = new LatestFrameScheduler<number>(
      (callback) => {
        callbacks.push(callback);
        return callbacks.length;
      },
      (value) => { committed.push(value); },
    );

    scheduler.schedule(1);
    scheduler.schedule(2);
    scheduler.schedule(3);
    expect(callbacks).toHaveLength(1);
    callbacks[0]!();

    expect(committed).toEqual([3]);
    expect(scheduler.pending).toBeFalse();
  });

  test("supports reusing a mutable latest value without copying it", () => {
    const callbacks: Array<() => void> = [];
    const value = { x: 1, y: 2 };
    const committed: Array<typeof value> = [];
    const scheduler = new LatestFrameScheduler<typeof value>(
      (callback) => {
        callbacks.push(callback);
        return callbacks.length;
      },
      (next) => { committed.push(next); },
    );

    scheduler.schedule(value);
    value.x = 3;
    value.y = 4;
    scheduler.schedule(value);
    callbacks[0]!();

    expect(committed).toHaveLength(1);
    expect(committed[0]).toBe(value);
    expect(committed[0]).toEqual({ x: 3, y: 4 });
  });

  test("cancels a pending latest value before it reaches the commit", () => {
    const callbacks: Array<() => void> = [];
    const cancelled: number[] = [];
    const committed: number[] = [];
    const scheduler = new LatestFrameScheduler<number>(
      (callback) => {
        callbacks.push(callback);
        return callbacks.length;
      },
      (value) => { committed.push(value); },
      (handle) => { cancelled.push(handle); },
    );

    scheduler.schedule(7);
    scheduler.cancel();
    callbacks[0]!();

    expect(cancelled).toEqual([1]);
    expect(committed).toEqual([]);
    expect(scheduler.pending).toBeFalse();
  });
});
