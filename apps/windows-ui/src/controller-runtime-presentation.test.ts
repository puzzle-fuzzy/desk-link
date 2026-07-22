import { describe, expect, test } from "bun:test";

import {
  advanceControllerRemoteSurfaceState,
  controllerRuntimeSurfaceChanged,
} from "./controller-runtime-presentation";

describe("controller runtime presentation", () => {
  test("keeps the connected surface when only status copy changes", () => {
    expect(controllerRuntimeSurfaceChanged(
      { state: "connected", streamId: 4 },
      { state: "connected", streamId: 4 },
    )).toBe(false);
  });

  test("rebuilds the surface when the connection state changes", () => {
    expect(controllerRuntimeSurfaceChanged(
      { state: "connected", streamId: 4 },
      { state: "reconnecting", streamId: 4 },
    )).toBe(true);
  });

  test("rebuilds the surface when the video stream changes", () => {
    expect(controllerRuntimeSurfaceChanged(
      { state: "connected", streamId: 4 },
      { state: "connected", streamId: 5 },
    )).toBe(true);
  });

  test("retains a connected surface during a transient reconnect", () => {
    expect(advanceControllerRemoteSurfaceState(
      "live",
      { state: "connected", streamId: 4 },
      { state: "reconnecting", streamId: null },
      true,
    )).toBe("retaining");
  });

  test("retains a connected surface while the retry handshake starts", () => {
    expect(advanceControllerRemoteSurfaceState(
      "retaining",
      { state: "connected", streamId: 4 },
      { state: "connecting", streamId: null },
      true,
    )).toBe("retaining");
  });

  test("does not retain a surface when reconnecting before the first frame", () => {
    expect(advanceControllerRemoteSurfaceState(
      "empty",
      { state: "connecting", streamId: null },
      { state: "reconnecting", streamId: null },
      false,
    )).toBe("empty");
  });

  test("keeps the retained surface through the full retry handshake", () => {
    let surface: "empty" | "live" | "retaining" = "live";
    const connected = { state: "connected", streamId: 4 };
    surface = advanceControllerRemoteSurfaceState(surface, connected, { state: "reconnecting", streamId: null }, true);
    surface = advanceControllerRemoteSurfaceState(surface, { state: "reconnecting", streamId: null }, { state: "connecting", streamId: null }, true);
    surface = advanceControllerRemoteSurfaceState(surface, { state: "connecting", streamId: null }, { state: "waitingApproval", streamId: null }, true);
    expect(surface).toBe("retaining");
    expect(advanceControllerRemoteSurfaceState(surface, { state: "waitingApproval", streamId: null }, connected, true)).toBe("live");
  });

  test("can retain a frame that arrived after the connected status", () => {
    const live = advanceControllerRemoteSurfaceState(
      "empty",
      { state: "connecting", streamId: null },
      { state: "connected", streamId: 4 },
      false,
    );
    expect(live).toBe("live");
    expect(advanceControllerRemoteSurfaceState(
      live,
      { state: "connected", streamId: 4 },
      { state: "reconnecting", streamId: null },
      true,
    )).toBe("retaining");
  });

  test("releases the surface on terminal stop", () => {
    expect(advanceControllerRemoteSurfaceState(
      "retaining",
      { state: "reconnecting", streamId: null },
      { state: "stopped", streamId: null },
      true,
    )).toBe("empty");
  });
});
