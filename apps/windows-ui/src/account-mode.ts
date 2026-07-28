export interface AccountModeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

const LOCAL_MODE_KEY = "desklink.account.local-mode";

function browserStorage(): AccountModeStorage | null {
  if (typeof window === "undefined") {
    return null;
  }
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function accountLocalModeEnabled(storage: AccountModeStorage | null = browserStorage()): boolean {
  if (!storage) {
    return false;
  }
  try {
    return storage.getItem(LOCAL_MODE_KEY) === "enabled";
  } catch {
    return false;
  }
}

export function setAccountLocalModeEnabled(
  enabled: boolean,
  storage: AccountModeStorage | null = browserStorage(),
): void {
  if (!storage) {
    return;
  }
  try {
    if (enabled) {
      storage.setItem(LOCAL_MODE_KEY, "enabled");
    } else {
      storage.removeItem(LOCAL_MODE_KEY);
    }
  } catch {
    // A blocked WebView storage must not prevent the local connection mode.
  }
}
