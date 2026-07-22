export type RemoteScaleMode = "fit" | "actual";

export type RemoteScaleStorage = Pick<Storage, "getItem" | "setItem">;

export const REMOTE_SCALE_STORAGE_KEY = "desklink.remoteScaleMode.v1";

/**
 * Keeps one received canvas pixel mapped to one physical display pixel when
 * Windows display scaling is enabled. Without this conversion, a 1:1 canvas
 * can be interpolated again by the WebView when its device pixel ratio is
 * above 1.
 */
export function nativeCanvasCssSize(pixelSize: number, devicePixelRatio: number): number {
  if (!Number.isFinite(pixelSize) || pixelSize <= 0) {
    return 0;
  }
  const ratio = Number.isFinite(devicePixelRatio) && devicePixelRatio > 0
    ? Math.max(1, devicePixelRatio)
    : 1;
  return pixelSize / ratio;
}

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
