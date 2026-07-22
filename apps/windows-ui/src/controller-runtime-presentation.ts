export interface ControllerRuntimeIdentity {
  state: string;
  streamId: number | null;
}

export type ControllerRemoteSurfaceState = "empty" | "live" | "retaining";

const RETAINABLE_RUNTIME_STATES = [
  "finding",
  "connecting",
  "waitingApproval",
  "reconnecting",
] as const;

/**
 * Runtime title/detail changes only affect the status badge. Rebuilding the
 * connected surface for those changes would tear down the WebCodecs decoder.
 */
export function controllerRuntimeSurfaceChanged(
  previous: ControllerRuntimeIdentity | null | undefined,
  next: ControllerRuntimeIdentity,
): boolean {
  return !previous || previous.state !== next.state || previous.streamId !== next.streamId;
}

/**
 * Advances the rendering lifecycle without touching DOM or decoder state.
 *
 * A connected session can receive its first frame after the connected status
 * arrives, so `live` intentionally does not require `hasSurface`. Once a
 * transient retry starts, the last frame is retained only when a real video
 * surface already exists. Terminal states always release the retained frame.
 */
export function advanceControllerRemoteSurfaceState(
  previousState: ControllerRemoteSurfaceState,
  previous: ControllerRuntimeIdentity | null | undefined,
  next: ControllerRuntimeIdentity,
  hasSurface: boolean,
): ControllerRemoteSurfaceState {
  if (next.state === "connected") {
    return "live";
  }
  if (
    hasSurface
    && (previousState === "live"
      || previousState === "retaining"
      || previous?.state === "connected")
    && RETAINABLE_RUNTIME_STATES.includes(next.state as typeof RETAINABLE_RUNTIME_STATES[number])
  ) {
    return "retaining";
  }
  return "empty";
}
