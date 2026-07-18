import { describe, expect, test } from "bun:test";
import { RemoteDisplaySwitchState } from "./remote-display-switch";

describe("remote display switch state", () => {
  test("accepts only an available display different from the active one", () => {
    const state = new RemoteDisplaySwitchState();

    expect(state.begin(1, 0, [0, 1])).toBe(true);
    expect(state.pendingId).toBe(1);
    expect(state.begin(0, 0, [0, 1])).toBe(false);
    expect(state.begin(2, 0, [0, 1])).toBe(false);
  });

  test("distinguishes an applied acknowledgement from a host rejection", () => {
    const applied = new RemoteDisplaySwitchState();
    applied.begin(1, 0, [0, 1]);
    expect(applied.acknowledge(1)).toBe("applied");
    expect(applied.pendingId).toBeNull();

    const rejected = new RemoteDisplaySwitchState();
    rejected.begin(1, 0, [0, 1]);
    expect(rejected.acknowledge(0)).toBe("rejected");
    expect(rejected.pendingId).toBeNull();
  });

  test("ignores stale failures and resets a live request", () => {
    const state = new RemoteDisplaySwitchState();
    state.begin(2, 0, [0, 1, 2]);

    expect(state.fail(1)).toBe(false);
    expect(state.pendingId).toBe(2);
    expect(state.fail(2)).toBe(true);
    expect(state.pendingId).toBeNull();
    expect(state.acknowledge(0)).toBe("idle");
  });
});
