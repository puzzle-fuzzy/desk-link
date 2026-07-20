export type FileQueueRecoveryState = "empty" | "protected" | "memoryOnly";

export interface FileQueueProtectionPresentation {
  tone: "protected" | "warning";
  message: string;
  retryLabel: string | null;
  retryDisabled: boolean;
}

export function fileQueueProtectionPresentation(
  state: FileQueueRecoveryState,
  recoveryMessage: string | null,
  retryBusy: boolean,
): FileQueueProtectionPresentation | null {
  if (state === "empty") return null;
  if (state === "protected") {
    return {
      tone: "protected",
      message: "等待队列已由当前 Windows 账户加密保存",
      retryLabel: null,
      retryDisabled: false,
    };
  }
  return {
    tone: "warning",
    message: recoveryMessage?.trim() || "等待队列仅保留到本次运行结束。",
    retryLabel: retryBusy ? "正在重试…" : "重试保护",
    retryDisabled: retryBusy,
  };
}
