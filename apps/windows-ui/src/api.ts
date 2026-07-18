import { Channel, invoke } from "@tauri-apps/api/core";

import type {
  ConnectionSettingsInput,
  ControllerDeviceInput,
  StreamBoundControllerInput,
  ControllerRenderMetrics,
  ControllerSignal,
  ControllerSnapshot,
  DiagnosticExportResult,
  DiagnosticUploadResult,
  FixedAccessSummary,
  HostSnapshot,
  PairingSessionSummary,
  RelayProbeInput,
  RelayProbeResult,
  RevocationResult,
  SavedDeviceInput,
  SavedDeviceRenameInput,
  WindowsPreferencesSummary,
} from "./types";
import type { VideoQualityPreference } from "./types";

export interface ControllerChannels {
  signals: Channel<ControllerSignal>;
  video: Channel<ArrayBuffer | ArrayBufferView | number[]>;
  audio: Channel<ArrayBuffer | ArrayBufferView | number[]>;
}

export function getHostSnapshot(): Promise<HostSnapshot> {
  return invoke<HostSnapshot>("get_host_snapshot");
}

export function getWindowsPreferences(): Promise<WindowsPreferencesSummary> {
  return invoke<WindowsPreferencesSummary>("get_windows_preferences");
}

export function setLaunchAtLogin(enabled: boolean): Promise<WindowsPreferencesSummary> {
  return invoke<WindowsPreferencesSummary>("set_launch_at_login", { enabled });
}

export function setDiagnosticsSharing(enabled: boolean): Promise<WindowsPreferencesSummary> {
  return invoke<WindowsPreferencesSummary>("set_diagnostics_sharing", { enabled });
}

export function uploadDiagnosticsNow(): Promise<DiagnosticUploadResult> {
  return invoke<DiagnosticUploadResult>("upload_diagnostics_now");
}

export function quitDeskLink(): Promise<void> {
  return invoke<void>("quit_desklink");
}

export function restartHost(): Promise<HostSnapshot> {
  return invoke<HostSnapshot>("restart_host");
}

export function respondHostApproval(requestId: number, allow: boolean): Promise<void> {
  return invoke<void>("respond_host_approval", { requestId, allow });
}

export function exportDiagnosticReport(): Promise<DiagnosticExportResult> {
  return invoke<DiagnosticExportResult>("export_diagnostic_report");
}

export function saveConnectionSettings(
  input: ConnectionSettingsInput,
): Promise<HostSnapshot> {
  return invoke<HostSnapshot>("save_connection_settings", { input });
}

export function setupManagedConnection(): Promise<HostSnapshot> {
  return invoke<HostSnapshot>("setup_managed_connection");
}

export function startPairingSession(): Promise<PairingSessionSummary> {
  return invoke<PairingSessionSummary>("start_pairing_session");
}

export function cancelPairingSession(): Promise<HostSnapshot> {
  return invoke<HostSnapshot>("cancel_pairing_session");
}

export function getFixedAccessPassword(): Promise<FixedAccessSummary> {
  return invoke<FixedAccessSummary>("get_fixed_access_password");
}

export function regenerateFixedAccessPassword(): Promise<FixedAccessSummary> {
  return invoke<FixedAccessSummary>("regenerate_fixed_access_password");
}

export function disableFixedAccessPassword(): Promise<HostSnapshot> {
  return invoke<HostSnapshot>("disable_fixed_access_password");
}

export function probeRelay(input: RelayProbeInput): Promise<RelayProbeResult> {
  return invoke<RelayProbeResult>("probe_relay", { input });
}

export function revokeTrustedController(
  fingerprint: string,
): Promise<RevocationResult> {
  return invoke<RevocationResult>("revoke_trusted_controller", { fingerprint });
}

export function getControllerSnapshot(): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("get_controller_snapshot");
}

export function createControllerChannels(
  onSignal: (signal: ControllerSignal) => void,
  onVideo: (payload: ArrayBuffer | ArrayBufferView | number[]) => void,
  onVideoError?: (error: unknown) => void,
  onAudio?: (payload: ArrayBuffer | ArrayBufferView | number[]) => void,
  onAudioError?: (error: unknown) => void,
): ControllerChannels {
  return {
    signals: new Channel<ControllerSignal>(onSignal),
    video: new Channel<ArrayBuffer | ArrayBufferView | number[]>((payload) => {
      try {
        onVideo(payload);
      } catch (error) {
        // Tauri advances a channel only after its callback returns. Never let one
        // malformed frame permanently block all following video messages.
        onVideoError?.(error);
      }
    }),
    audio: new Channel<ArrayBuffer | ArrayBufferView | number[]>((payload) => {
      try {
        onAudio?.(payload);
      } catch (error) {
        // Audio is optional. A malformed packet must not stall Tauri's channel
        // or interrupt the live video and input session.
        onAudioError?.(error);
      }
    }),
  };
}

export function connectDevice(
  input: ControllerDeviceInput,
  channels: ControllerChannels,
): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("connect_device", { input, ...channels });
}

export function connectSavedDevice(
  input: SavedDeviceInput,
  channels: ControllerChannels,
): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("connect_saved_device", { input, ...channels });
}

export function reconnectController(
  channels: ControllerChannels,
): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("reconnect_controller", { ...channels });
}

export function sendControllerInput(input: StreamBoundControllerInput): Promise<void> {
  return invoke<void>("send_controller_input", { input });
}

export function sendControllerText(text: string): Promise<void> {
  return invoke<void>("send_controller_text", { text });
}

export function setControllerAudioEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_controller_audio_enabled", { enabled });
}

export function setControllerVideoQuality(preference: VideoQualityPreference): Promise<void> {
  return invoke<void>("set_controller_video_quality", { preference });
}

export function sendControllerClipboard(): Promise<void> {
  return invoke<void>("send_controller_clipboard");
}

export function requestControllerClipboard(): Promise<void> {
  return invoke<void>("request_controller_clipboard");
}

export function chooseAndSendControllerFile(): Promise<void> {
  return invoke<void>("choose_and_send_controller_file");
}

export function queueControllerFiles(paths: string[]): Promise<void> {
  return invoke<void>("queue_controller_files", { paths });
}

export function removeControllerQueuedFile(transferId: string): Promise<void> {
  return invoke<void>("remove_controller_queued_file", { transferId });
}

export function clearControllerFileQueue(): Promise<void> {
  return invoke<void>("clear_controller_file_queue");
}

export function resumeControllerFileQueue(): Promise<void> {
  return invoke<void>("resume_controller_file_queue");
}

export function requestControllerRemoteFile(): Promise<void> {
  return invoke<void>("request_controller_remote_file");
}

export function retryControllerFile(): Promise<void> {
  return invoke<void>("retry_controller_file");
}

export function cancelControllerFile(): Promise<void> {
  return invoke<void>("cancel_controller_file");
}

export function openControllerDownloadsFolder(): Promise<void> {
  return invoke<void>("open_controller_downloads_folder");
}

export function requestControllerKeyframe(): Promise<void> {
  return invoke<void>("request_controller_keyframe");
}

export function reportControllerRenderMetrics(metrics: ControllerRenderMetrics): Promise<void> {
  return invoke<void>("report_controller_render_metrics", { metrics });
}

export function openGithubRepository(): Promise<void> {
  return invoke<void>("open_github_repository");
}

export function selectControllerDisplay(displayId: number): Promise<void> {
  return invoke<void>("select_controller_display", { displayId });
}

export function disconnectController(): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("disconnect_controller");
}

export function forgetSavedDevice(input: SavedDeviceInput): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("forget_saved_device", { input });
}

export function renameSavedDevice(input: SavedDeviceRenameInput): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("rename_saved_device", { input });
}

export function clearSavedDevices(): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("clear_saved_devices");
}
