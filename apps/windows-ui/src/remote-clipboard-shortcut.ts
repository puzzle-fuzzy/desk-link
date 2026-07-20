export interface RemoteClipboardShortcutInput {
  altKey: boolean;
  code: string;
  ctrlKey: boolean;
  key: string;
  metaKey: boolean;
  repeat: boolean;
  shiftKey: boolean;
}

/**
 * Ctrl+V is treated as an explicit request to type the controller's local
 * clipboard into the remote desktop. Alt is excluded so Windows AltGr
 * keyboard layouts continue to produce their normal characters.
 */
export function isRemoteClipboardPasteShortcut(
  input: RemoteClipboardShortcutInput,
): boolean {
  const pasteKey = input.code === "KeyV" || input.key.toLowerCase() === "v";
  return input.ctrlKey
    && !input.altKey
    && !input.metaKey
    && !input.repeat
    && pasteKey;
}
