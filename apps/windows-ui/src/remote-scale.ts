export type RemoteScaleMode = "fit" | "actual";

export type RemoteScaleStorage = Pick<Storage, "getItem" | "setItem">;

export const REMOTE_SCALE_STORAGE_KEY = "desklink.remoteScaleMode.v1";

export function normalizeRemoteScaleMode(value: unknown): RemoteScaleMode {
  return value === "actual" ? "actual" : "fit";
}

export function loadRemoteScaleMode(storage: RemoteScaleStorage | null = browserStorage()): RemoteScaleMode {
  if (!storage) {
    return "fit";
  }
  try {
    return normalizeRemoteScaleMode(storage.getItem(REMOTE_SCALE_STORAGE_KEY));
  } catch {
    return "fit";
  }
}

export function saveRemoteScaleMode(
  mode: RemoteScaleMode,
  storage: RemoteScaleStorage | null = browserStorage(),
): boolean {
  if (!storage) {
    return false;
  }
  try {
    storage.setItem(REMOTE_SCALE_STORAGE_KEY, mode);
    return true;
  } catch {
    return false;
  }
}

function browserStorage(): RemoteScaleStorage | null {
  try {
    return typeof globalThis.localStorage === "undefined" ? null : globalThis.localStorage;
  } catch {
    return null;
  }
}
