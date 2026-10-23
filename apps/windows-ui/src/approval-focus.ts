/** -1 focuses the dialog itself when all approval actions are disabled. */
export function approvalTabFocusIndex(
  enabledControlCount: number,
  currentIndex: number,
  shiftKey: boolean,
): number | null {
  if (enabledControlCount === 0) return -1;
  if (currentIndex < 0) return shiftKey ? enabledControlCount - 1 : 0;
  if (shiftKey && currentIndex === 0) return enabledControlCount - 1;
  if (!shiftKey && currentIndex === enabledControlCount - 1) return 0;
  return null;
}

export function approvalNeedsFocus(
  requestId: number,
  focusedRequestId: number | null,
  focusInsideDialog: boolean,
): boolean {
  return requestId !== focusedRequestId || !focusInsideDialog;
}
