import type { ControllerSnapshot } from "./types";

/**
 * An invoke result and the controller status channel can complete in either
 * order. Keep the channel's runtime state when it advanced while the invoke
 * was in flight, while taking the rest of the fresh snapshot from the invoke.
 */
export function mergeControllerOperationSnapshot(
  operationSnapshot: ControllerSnapshot,
  currentSnapshot: ControllerSnapshot | null,
  operationSignalRevision: number,
  currentSignalRevision: number,
): ControllerSnapshot {
  if (!currentSnapshot || operationSignalRevision === currentSignalRevision) {
    return operationSnapshot;
  }
  return {
    ...operationSnapshot,
    runtime: currentSnapshot.runtime,
  };
}
