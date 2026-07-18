export type FileTransferTerminalState = "completed" | "failed" | "rejected" | "cancelled";

export type FileTransferHistorySource = {
  state: "waiting" | "sending" | "receiving" | "verifying" | FileTransferTerminalState;
  direction: "upload" | "download";
  name: string;
  total: number;
  message: string;
};

export type TransferHistoryEntry = {
  id: number;
  direction: FileTransferHistorySource["direction"];
  name: string;
  size: number;
  state: FileTransferTerminalState;
  finishedAtMs: number;
};

export function isFileTransferTerminal(
  state: FileTransferHistorySource["state"],
): state is FileTransferTerminalState {
  return state === "completed" || state === "failed" || state === "rejected" || state === "cancelled";
}

export function appendTransferHistory(
  history: TransferHistoryEntry[],
  previous: FileTransferHistorySource | null,
  next: FileTransferHistorySource,
  id: number,
  finishedAtMs: number,
  limit = 8,
): TransferHistoryEntry[] {
  if (!isFileTransferTerminal(next.state)) return history;
  if (
    previous
    && isFileTransferTerminal(previous.state)
    && previous.state === next.state
    && previous.direction === next.direction
    && previous.name === next.name
    && previous.message === next.message
  ) {
    return history;
  }
  return [{
    id,
    direction: next.direction,
    name: next.name,
    size: next.total,
    state: next.state,
    finishedAtMs,
  }, ...history].slice(0, limit);
}
