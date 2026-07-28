import { describe, expect, test } from "bun:test";

import {
  accountLocalModeEnabled,
  setAccountLocalModeEnabled,
  type AccountModeStorage,
} from "./account-mode";

class MemoryStorage implements AccountModeStorage {
  private readonly values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }
}

test("本机模式偏好可以保存并清除", () => {
  const storage = new MemoryStorage();
  expect(accountLocalModeEnabled(storage)).toBe(false);

  setAccountLocalModeEnabled(true, storage);
  expect(accountLocalModeEnabled(storage)).toBe(true);

  setAccountLocalModeEnabled(false, storage);
  expect(accountLocalModeEnabled(storage)).toBe(false);
});

describe("本机模式存储失败", () => {
  test("读取失败时安全回退为未启用", () => {
    const storage: AccountModeStorage = {
      getItem: () => {
        throw new Error("storage blocked");
      },
      setItem: () => {},
      removeItem: () => {},
    };
    expect(accountLocalModeEnabled(storage)).toBe(false);
  });

  test("写入失败不会阻断继续使用", () => {
    const storage: AccountModeStorage = {
      getItem: () => null,
      setItem: () => {
        throw new Error("storage blocked");
      },
      removeItem: () => {
        throw new Error("storage blocked");
      },
    };
    expect(() => setAccountLocalModeEnabled(true, storage)).not.toThrow();
    expect(() => setAccountLocalModeEnabled(false, storage)).not.toThrow();
  });
});
