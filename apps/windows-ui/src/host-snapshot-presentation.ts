import type { HostSnapshot } from "./types";

export function hostSnapshotPresentationChanged(
  previous: HostSnapshot | null,
  next: HostSnapshot | null,
): boolean {
  return JSON.stringify(presentation(previous)) !== JSON.stringify(presentation(next));
}

// The refresh timestamp is not visible on the controller page. Ignoring it
// avoids rebuilding the remote canvas and its decoder for unchanged reads.
function presentation(snapshot: HostSnapshot | null): unknown {
  if (!snapshot) return null;
  return [
    snapshot.readiness,
    snapshot.title,
    snapshot.detail,
    snapshot.runtime,
    snapshot.connection,
    snapshot.connectionError,
    snapshot.trustedControllers,
    snapshot.trustedError,
    snapshot.relayStatus,
    snapshot.diagnosticChecks,
    snapshot.pairingActive,
    snapshot.pendingApproval,
    snapshot.fixedPasswordEnabled,
    snapshot.fixedPasswordError,
    snapshot.deviceId,
  ];
}
