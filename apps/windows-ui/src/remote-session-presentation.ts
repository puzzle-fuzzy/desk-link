import type { VideoPathKind, VideoQualityPreference, VideoQualityPreset } from "./types";

export const REMOTE_TOOLBAR_IDLE_MS = 3_000;

export type RemoteToolbarVisibilityInput = {
  connected: boolean;
  fullscreen: boolean;
  nowMs: number;
  lastRevealedAtMs: number;
  pointerNearTop: boolean;
  toolbarFocused: boolean;
  panelOpen: boolean;
};

export function remoteToolbarVisible(input: RemoteToolbarVisibilityInput): boolean {
  if (!input.connected || !input.fullscreen) {
    return true;
  }
  if (input.pointerNearTop || input.toolbarFocused || input.panelOpen) {
    return true;
  }
  return input.nowMs - input.lastRevealedAtMs < REMOTE_TOOLBAR_IDLE_MS;
}

export function remoteToolbarHideDelay(input: RemoteToolbarVisibilityInput): number | null {
  if (!input.connected || !input.fullscreen) {
    return null;
  }
  if (input.pointerNearTop || input.toolbarFocused || input.panelOpen) {
    return null;
  }
  return Math.max(0, REMOTE_TOOLBAR_IDLE_MS - (input.nowMs - input.lastRevealedAtMs));
}

function presetLabel(preset: VideoQualityPreset): string {
  return preset === "smooth" ? "流畅" : preset === "balanced" ? "均衡" : "清晰";
}

export function remoteSessionSummary(
  width: number,
  height: number,
  preference: VideoQualityPreference,
  preset: VideoQualityPreset,
): string {
  const quality = preference === "automatic"
    ? `自动（${presetLabel(preset)}）`
    : presetLabel(preference);
  return `${width} × ${height} · ${quality} · 已加密`;
}

export function remoteVideoPathLabel(path: VideoPathKind): string {
  return path === "directLan" ? "局域网直连" : "公网中继";
}

export function remoteVideoPathDescription(path: VideoPathKind): string {
  return path === "directLan"
    ? "视频正在通过已认证的局域网直连传输；控制通道仍保持端到端加密。"
    : "视频正在通过 DeskLink 公网中继传输；中继只转发加密数据。";
}
