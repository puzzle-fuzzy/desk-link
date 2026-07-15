import { Channel, invoke } from "@tauri-apps/api/core";

import type {
  ConnectionSettingsInput,
  ControllerConnectionInput,
  ControllerInput,
  ControllerSignal,
  ControllerSnapshot,
  DiagnosticExportResult,
  HostSnapshot,
  PairingSessionSummary,
  RelayProbeInput,
  RelayProbeResult,
  RevocationResult,
} from "./types";

export interface ControllerChannels {
  signals: Channel<ControllerSignal>;
  video: Channel<ArrayBuffer | Uint8Array | number[]>;
}

export function getHostSnapshot(): Promise<HostSnapshot> {
  return invoke<HostSnapshot>("get_host_snapshot");
}

export function exportDiagnosticReport(): Promise<DiagnosticExportResult> {
  return invoke<DiagnosticExportResult>("export_diagnostic_report");
}

export function saveConnectionSettings(
  input: ConnectionSettingsInput,
): Promise<HostSnapshot> {
  return invoke<HostSnapshot>("save_connection_settings", { input });
}

export function startPairingSession(): Promise<PairingSessionSummary> {
  return invoke<PairingSessionSummary>("start_pairing_session");
}

export function cancelPairingSession(): Promise<HostSnapshot> {
  return invoke<HostSnapshot>("cancel_pairing_session");
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
  onVideo: (payload: ArrayBuffer | Uint8Array | number[]) => void,
): ControllerChannels {
  return {
    signals: new Channel<ControllerSignal>(onSignal),
    video: new Channel<ArrayBuffer | Uint8Array | number[]>(onVideo),
  };
}

export function connectController(
  input: ControllerConnectionInput,
  channels: ControllerChannels,
): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("connect_controller", { input, ...channels });
}

export function reconnectController(
  channels: ControllerChannels,
): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("reconnect_controller", { ...channels });
}

export function sendControllerInput(input: ControllerInput): Promise<void> {
  return invoke<void>("send_controller_input", { input });
}

export function requestControllerKeyframe(): Promise<void> {
  return invoke<void>("request_controller_keyframe");
}

export function disconnectController(): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("disconnect_controller");
}

export function forgetController(): Promise<ControllerSnapshot> {
  return invoke<ControllerSnapshot>("forget_controller");
}
