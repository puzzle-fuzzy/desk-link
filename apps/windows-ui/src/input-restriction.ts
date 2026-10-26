import type { ControllerRuntimeSummary, InputRestrictionReason } from "./types";

export function inputRestrictionPresentation(reason: InputRestrictionReason | null): { title: string; detail: string } | null {
  switch (reason) {
    case "blocked": return { title: "远端输入受限", detail: "请让被控电脑用户切回普通桌面。管理员窗口、UAC 或锁屏可能需要在被控电脑本机处理；画面连接仍保持。" };
    case "unsupportedKey": return { title: "此按键当前不受支持", detail: "请尝试快捷键菜单，或在被控电脑本机完成此操作。" };
    case "invalidInput": return { title: "部分输入未被远端接受", detail: "请重新点击远程画面后重试。远端恢复接受输入时，此提示会自动消失。" };
    case null: return null;
  }
}

export function restrictionAfterRuntime(
  reason: InputRestrictionReason | null,
  previous: ControllerRuntimeSummary | undefined,
  next: ControllerRuntimeSummary,
): InputRestrictionReason | null {
  return next.state !== "connected" || previous?.streamId !== next.streamId ? null : reason;
}
