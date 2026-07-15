export type Readiness = "configured" | "setup" | "attention";

export interface ConnectionSummary {
  relayAddress: string;
  serverName: string;
  sessionId: string;
  streamId: number;
  hasSavedKey: boolean;
}

export interface TrustedControllerSummary {
  deviceId: string;
  verifyKey: string;
  fingerprint: string;
  approvedAtUnixS: number;
}

export interface LanAddressSummary {
  relayAddress: string;
  interfaceName: string;
  isPrimary: boolean;
}

export type RelayMode = "unconfigured" | "lan" | "external";
export type RelayState = "inactive" | "starting" | "ready" | "offline" | "failed";

export interface RelayStatusSummary {
  mode: RelayMode;
  state: RelayState;
  title: string;
  detail: string;
  port: number | null;
  addresses: LanAddressSummary[];
}

export type DiagnosticCheckStatus = "passed" | "warning" | "failed" | "notApplicable";

export interface DiagnosticCheckSummary {
  code: string;
  status: DiagnosticCheckStatus;
  title: string;
  detail: string;
}

export type HostRuntimeState =
  | "starting"
  | "pairing"
  | "connecting"
  | "connected"
  | "reconnecting"
  | "stopped"
  | "notConfigured";

export interface HostRuntimeSummary {
  state: HostRuntimeState;
  title: string;
  detail: string;
  tooltip: string;
}

export interface HostSnapshot {
  readiness: Readiness;
  title: string;
  detail: string;
  runtime: HostRuntimeSummary;
  connection: ConnectionSummary | null;
  connectionError: string | null;
  trustedControllers: TrustedControllerSummary[];
  trustedError: string | null;
  relayStatus: RelayStatusSummary;
  diagnosticChecks: DiagnosticCheckSummary[];
  pairingActive: boolean;
  refreshedAtUnixS: number;
}

export interface PairingSessionSummary {
  invitation: string;
  expiresAtUnixS: number;
}

export interface RevocationResult {
  revoked: boolean;
  snapshot: HostSnapshot;
}

export interface ConnectionSettingsInput {
  relayAddress: string;
  serverName: string;
  sessionId: string;
  relayKey: string;
  streamId: string;
}

export interface RelayProbeInput {
  relayAddress: string;
  serverName: string;
}

export interface RelayProbeResult {
  title: string;
  detail: string;
  relayAddress: string;
  elapsedMs: number;
}

export interface DiagnosticExportResult {
  reportId: string;
  fileName: string;
  filePath: string;
  checkCount: number;
}

export type ControllerRuntimeState =
  | "idle"
  | "connecting"
  | "waitingApproval"
  | "connected"
  | "reconnecting"
  | "stopped";

export interface ControllerRuntimeSummary {
  state: ControllerRuntimeState;
  title: string;
  detail: string;
  streamId: number | null;
}

export interface SavedControllerConnectionSummary {
  relayAddress: string;
  serverName: string;
  hostDeviceId: string;
  hostVerifyKey: string;
}

export interface ControllerSnapshot {
  runtime: ControllerRuntimeSummary;
  savedConnection: SavedControllerConnectionSummary | null;
  connectionError: string | null;
}

export interface ControllerConnectionInput {
  relayAddress: string;
  serverName: string;
  invitation: string;
}

export interface ControllerInput {
  kind: "mouseMove" | "mouseButton" | "wheel" | "key";
  x?: number;
  y?: number;
  deltaX?: number;
  deltaY?: number;
  button?: "left" | "right" | "middle";
  key?: string;
  character?: string;
  pressed?: boolean;
  modifiers?: number;
}

export interface ControllerVideoConfigSignal {
  kind: "videoConfig";
  streamId: number;
  configVersion: number;
  width: number;
  height: number;
  sequenceHeader: number[];
}

export type ControllerSignal =
  | { kind: "status"; runtime: ControllerRuntimeSummary }
  | ControllerVideoConfigSignal
  | {
      kind: "cursor";
      streamId: number;
      sequence: number;
      xMillionths: number;
      yMillionths: number;
      visible: boolean;
    }
  | {
      kind: "metrics";
      receivedVideoPackets: number;
      droppedVideoPackets: number;
      completedFrames: number;
    };
