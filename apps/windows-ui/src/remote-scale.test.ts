import { describe, expect, test } from "bun:test";

import {
  REMOTE_SCALE_STORAGE_KEY,
  loadRemoteScaleMode,
  nativeCanvasCssSize,
  normalizeRemoteScaleMode,
  saveRemoteScaleMode,
  type RemoteScaleStorage,
} from "./remote-scale";

describe("远程画面原始像素尺寸", () => {
  test("按 Windows 显示缩放比例换算 CSS 尺寸", () => {
    expect(nativeCanvasCssSize(1920, 1)).toBe(1920);
    expect(nativeCanvasCssSize(1920, 1.25)).toBe(1536);
  });

  test("对无效显示缩放比例安全回退", () => {
    expect(nativeCanvasCssSize(1920, 0)).toBe(1920);
    expect(nativeCanvasCssSize(1920, Number.NaN)).toBe(1920);
    expect(nativeCanvasCssSize(0, 1.25)).toBe(0);
  });
});

function memoryStorage(initial: string | null = null): RemoteScaleStorage & { value: string | null } {
  return {
    value: initial,
    getItem(key) {
      expect(key).toBe(REMOTE_SCALE_STORAGE_KEY);
      return this.value;
    },
    setItem(key, value) {
      expect(key).toBe(REMOTE_SCALE_STORAGE_KEY);
      this.value = value;
    },
  };
}

describe("remote scale preference", () => {
  test("defaults missing and unknown values to fit mode", () => {
    expect(normalizeRemoteScaleMode(null)).toBe("fit");
    expect(normalizeRemoteScaleMode("oversized")).toBe("fit");
    expect(loadRemoteScaleMode(memoryStorage())).toBe("fit");
  });

  test("restores and saves actual pixel mode", () => {
    const storage = memoryStorage("actual");
    expect(loadRemoteScaleMode(storage)).toBe("actual");
    expect(saveRemoteScaleMode("fit", storage)).toBe(true);
    expect(storage.value).toBe("fit");
  });

  test("keeps the remote session usable when storage is unavailable", () => {
    const failingStorage: RemoteScaleStorage = {
      getItem() {
        throw new Error("storage unavailable");
      },
      setItem() {
        throw new Error("storage unavailable");
      },
    };
    expect(loadRemoteScaleMode(failingStorage)).toBe("fit");
    expect(saveRemoteScaleMode("actual", failingStorage)).toBe(false);
  });
});
