export interface ControllerRuntimeIdentity {
  state: string;
  streamId: number | null;
}

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

export function canRetainRemoteSurface(
  previous: ControllerRuntimeIdentity | null | undefined,
  next: ControllerRuntimeIdentity,
  hasSurface: boolean,
): boolean {
  return hasSurface
    && previous?.state === "connected"
    && ["finding", "connecting", "waitingApproval", "reconnecting"].includes(next.state);
}
