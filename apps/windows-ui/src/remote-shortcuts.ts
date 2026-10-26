import type { ControllerKeyInput } from "./remote-keyboard-state";

export const REMOTE_SHORTCUTS = [
  { id: "escape", label: "发送 Esc", description: "取消远端操作 / 关闭远端菜单", key: "escape", modifiers: 0 },
  { id: "windows", label: "Windows 键", description: "远端开始菜单", key: "meta", modifiers: 0 },
  { id: "alt-tab", label: "Alt + Tab", description: "切换远端窗口", key: "tab", modifiers: 4 },
  { id: "alt-f4", label: "Alt + F4", description: "关闭远端窗口", key: "f4", modifiers: 4 },
  { id: "control-escape", label: "Ctrl + Esc", description: "远端开始菜单", key: "escape", modifiers: 2 },
  { id: "task-manager", label: "Ctrl + Shift + Esc", description: "远端任务管理器", key: "escape", modifiers: 3 },
  { id: "show-desktop", label: "Win + D", description: "显示远端桌面", key: "character", character: "d", modifiers: 8 },
  { id: "lock", label: "Win + L", description: "锁定远端电脑", key: "character", character: "l", modifiers: 8 },
] as const;

export function remoteShortcutInput(id: string): ControllerKeyInput | null {
  const shortcut = REMOTE_SHORTCUTS.find((entry) => entry.id === id);
  if (!shortcut) return null;
  return {
    kind: "key",
    key: shortcut.key,
    ...("character" in shortcut ? { character: shortcut.character } : {}),
    pressed: true,
    modifiers: shortcut.modifiers,
  };
}
