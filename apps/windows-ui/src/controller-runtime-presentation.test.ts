import { describe, expect, test } from "bun:test";

import { controllerRuntimeSurfaceChanged } from "./controller-runtime-presentation";

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
});
