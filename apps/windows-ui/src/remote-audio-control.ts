import type { RemoteAudioPlaybackSnapshot } from "./remote-audio";

export function remoteAudioControl(
  status: "starting" | "unavailable" | "enabled" | "muted",
  enabled: boolean,
  message: string,
  playback: Pick<RemoteAudioPlaybackSnapshot, "state" | "message">,
) {
  const unavailable = status === "starting" || status === "unavailable";
  const muted = status === "muted" || !enabled;
  const retry = status === "enabled" && enabled && ["suspended", "failed", "closed"].includes(playback.state);
  return {
    unavailable, muted, retry,
    iconName: unavailable || retry ? "volume-off" as const : muted ? "volume-x" as const : "volume-2" as const,
    label: status === "starting" ? "声音准备中" : status === "unavailable" ? "无声音" : retry ? "重试声音" : muted ? "打开声音" : "静音",
    message: status === "enabled" && enabled ? playback.message : message,
  };
}
