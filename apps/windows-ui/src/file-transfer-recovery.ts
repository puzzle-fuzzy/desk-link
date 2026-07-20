import type { ControllerSignal, TransferRecoverySummary } from "./types";

export type RecoveredFileTransfer = Extract<ControllerSignal, { kind: "fileTransfer" }>;

export function recoveredFileTransfer(
  recovery: TransferRecoverySummary,
): RecoveredFileTransfer {
  return {
    kind: "fileTransfer",
    state: "failed",
    direction: recovery.direction,
    name: recovery.name,
    transferred: 0,
    total: recovery.total,
    message: `${recovery.message} 目标设备：${recovery.deviceId}。`,
  };
}

export function preferredDeviceIdForRecovery(
  currentDraft: string,
  recoveryDeviceId: string | null,
  latestSavedDeviceId: string | null,
): string {
  if (currentDraft) return currentDraft;
  return recoveryDeviceId ?? latestSavedDeviceId ?? "";
}

export function fileRecoveryAvailabilityAfterSignal(
  current: boolean,
  state: RecoveredFileTransfer["state"],
): boolean {
  if (state === "waiting" || state === "sending" || state === "receiving" || state === "verifying") {
    return true;
  }
  if (state === "completed") {
    return false;
  }
  return current;
}
