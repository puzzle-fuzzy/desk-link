export type FileTransferState =
  | "waiting"
  | "sending"
  | "receiving"
  | "verifying"
  | "completed"
  | "failed"
  | "rejected"
  | "cancelled";

export interface TransferStatusIdentity {
  state: FileTransferState;
  direction: "upload" | "download";
  name: string;
  total: number;
}

export interface TransferActivityState {
  unreadResults: number;
  tone: "success" | "error" | null;
}

export const TRANSFER_PROGRESS_PAINT_INTERVAL_MS = 100;

export function transferProgressPaintDelay(
  previousState: FileTransferState | null,
  nextState: FileTransferState,
  lastPaintAtMs: number | null,
  nowMs: number,
): number {
  if (
    previousState !== nextState
    || isTerminalTransferState(nextState)
    || nextState === "waiting"
    || nextState === "verifying"
    || lastPaintAtMs === null
    || !Number.isFinite(nowMs)
    || !Number.isFinite(lastPaintAtMs)
    || nowMs < lastPaintAtMs
  ) {
    return 0;
  }
  const elapsed = nowMs - lastPaintAtMs;
  return Math.max(0, TRANSFER_PROGRESS_PAINT_INTERVAL_MS - elapsed);
}

export function recordTransferResult(
  activity: TransferActivityState,
  previous: TransferStatusIdentity | null,
  next: TransferStatusIdentity,
  panelOpen: boolean,
): TransferActivityState {
  if (!isTerminalTransferState(next.state) || sameTerminalResult(previous, next)) {
    return activity;
  }
  return {
    unreadResults: panelOpen ? 0 : Math.min(99, activity.unreadResults + 1),
    tone: next.state === "completed" ? "success" : "error",
  };
}

export function markTransferResultsRead(activity: TransferActivityState): TransferActivityState {
  return activity.unreadResults === 0 ? activity : { ...activity, unreadResults: 0 };
}

export function isTerminalTransferState(state: FileTransferState): boolean {
  return state === "completed" || state === "failed" || state === "rejected" || state === "cancelled";
}

function sameTerminalResult(
  previous: TransferStatusIdentity | null,
  next: TransferStatusIdentity,
): boolean {
  return previous?.state === next.state
    && previous.direction === next.direction
    && previous.name === next.name
    && previous.total === next.total;
}
