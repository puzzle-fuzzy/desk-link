import { describe, expect, test } from "bun:test";

import {
  canRetainRemoteSurface,
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
    expect(canRetainRemoteSurface(
      { state: "connected", streamId: 4 },
      { state: "reconnecting", streamId: null },
      true,
    )).toBe(true);
  });

  test("retains a connected surface while the retry handshake starts", () => {
    expect(canRetainRemoteSurface(
      { state: "connected", streamId: 4 },
      { state: "connecting", streamId: null },
      true,
    )).toBe(true);
  });

  test("does not retain a surface when reconnecting before the first frame", () => {
    expect(canRetainRemoteSurface(
      { state: "connecting", streamId: null },
      { state: "reconnecting", streamId: null },
      false,
    )).toBe(false);
  });
});
