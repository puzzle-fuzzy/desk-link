import type { VideoQualityPreference, VideoQualityPreset } from "./types";

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
