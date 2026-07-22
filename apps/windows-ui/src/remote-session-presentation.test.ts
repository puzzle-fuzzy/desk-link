import { describe, expect, test } from "bun:test";

import {
  REMOTE_TOOLBAR_IDLE_MS,
  remoteSessionSummary,
  remoteToolbarHideDelay,
  remoteToolbarVisible,
} from "./remote-session-presentation";

const base = {
  connected: true,
  fullscreen: true,
  nowMs: 4_000,
  lastRevealedAtMs: 1_000,
  pointerNearTop: false,
  toolbarFocused: false,
  panelOpen: false,
};

describe("远程会话工具栏", () => {
  test("普通窗口和非连接状态始终保持可见", () => {
    expect(remoteToolbarVisible({ ...base, fullscreen: false })).toBe(true);
    expect(remoteToolbarVisible({ ...base, connected: false })).toBe(true);
  });

  test("全屏显示三秒后收起", () => {
    expect(REMOTE_TOOLBAR_IDLE_MS).toBe(3_000);
    expect(remoteToolbarVisible({ ...base, nowMs: 3_999 })).toBe(true);
    expect(remoteToolbarVisible(base)).toBe(false);
    expect(remoteToolbarHideDelay({ ...base, nowMs: 2_250 })).toBe(1_750);
  });

  test("顶部触发、工具栏焦点和面板打开都会固定显示", () => {
    expect(remoteToolbarVisible({ ...base, pointerNearTop: true })).toBe(true);
    expect(remoteToolbarVisible({ ...base, toolbarFocused: true })).toBe(true);
    expect(remoteToolbarVisible({ ...base, panelOpen: true })).toBe(true);
    expect(remoteToolbarHideDelay({ ...base, panelOpen: true })).toBeNull();
  });
});

describe("远程会话状态摘要", () => {
  test("自动画质只显示用户需要的当前档位", () => {
    expect(remoteSessionSummary(1_920, 1_080, "automatic", "sharp"))
      .toBe("1920 × 1080 · 自动（清晰） · 已加密");
    expect(remoteSessionSummary(1_280, 720, "automatic", "smooth"))
      .toBe("1280 × 720 · 自动（流畅） · 已加密");
  });

  test("手动画质不伪装成自动状态", () => {
    expect(remoteSessionSummary(1_920, 1_080, "balanced", "balanced"))
      .toBe("1920 × 1080 · 均衡 · 已加密");
  });
});
