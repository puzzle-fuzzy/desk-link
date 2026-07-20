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

export type RelayMode = "unconfigured" | "external";
export type RelayState = "inactive" | "ready";

export interface RelayStatusSummary {
  mode: RelayMode;
  state: RelayState;
  title: string;
  detail: string;
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
  | "available"
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

export interface HostApprovalSummary {
  requestId: number;
  deviceId: string;
  fingerprint: string;
  expiresAtUnixS: number;
  identityChanged: boolean;
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
  pendingApproval: HostApprovalSummary | null;
  fixedPasswordEnabled: boolean;
  fixedPasswordError: string | null;
  deviceId: string | null;
  refreshedAtUnixS: number;
}

export interface PairingSessionSummary {
  deviceId: string;
  temporaryPassword: string;
  expiresAtUnixS: number;
}

export interface FixedAccessSummary {
  deviceId: string;
  password: string;
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

export interface WindowsPreferencesSummary {
  launchAtLogin: boolean;
  diagnosticsSharingEnabled: boolean;
  closeToTray: boolean;
  interfaceLanguage: string;
  version: string;
}

export interface DiagnosticUploadResult {
  uploadedSources: number;
  uploadedEvents: number;
}

export type ControllerRuntimeState =
  | "idle"
  | "finding"
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
  deviceId: string;
  relayAddress: string;
  serverName: string;
  hostDeviceId: string;
  hostVerifyKey: string;
}

export interface SavedDeviceCredentialSummary {
  deviceId: string;
  alias: string | null;
  persistent: boolean;
  lastUsedUnixS: number;
}

export interface SavedDeviceRenameInput {
  deviceId: string;
  alias: string;
}

export interface TransferRecoverySummary {
  revision: number;
  deviceId: string;
  direction: "upload" | "download";
  name: string;
  total: number;
  message: string;
}

export interface FileQueueRecoverySummary {
  revision: number;
  deviceId: string;
  queued: Array<{
    id: string;
    name: string;
    size: number;
  }>;
  paused: boolean;
  message: string;
}

export interface ControllerSnapshot {
  runtime: ControllerRuntimeSummary;
  savedConnection: SavedControllerConnectionSummary | null;
  connectionError: string | null;
  savedDevices: SavedDeviceCredentialSummary[];
  savedDevicesError: string | null;
  fileRecovery: TransferRecoverySummary | null;
  fileRecoveryError: string | null;
  fileQueueRecovery: FileQueueRecoverySummary | null;
  fileQueueRecoveryError: string | null;
}

export interface ControllerDeviceInput {
  deviceId: string;
  temporaryPassword: string;
}

export interface SavedDeviceInput {
  deviceId: string;
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

export interface StreamBoundControllerInput extends ControllerInput {
  streamId: number;
}

export interface ControllerRenderMetrics {
  streamId: number;
  receivedFrames: number;
  submittedFrames: number;
  displayedFrames: number;
  malformedFrames: number;
  decoderRecoveries: number;
  firstFrameMs: number | null;
}

export interface ControllerVideoConfigSignal {
  kind: "videoConfig";
  streamId: number;
  configVersion: number;
  width: number;
  height: number;
  sequenceHeader: number[];
}

export interface RemoteDisplaySummary {
  id: number;
  width: number;
  height: number;
  primary: boolean;
}

export type VideoQualityPreset = "smooth" | "balanced" | "sharp";
export type VideoQualityPreference = "automatic" | VideoQualityPreset;

export type ControllerSignal =
  | { kind: "status"; runtime: ControllerRuntimeSummary }
  | ControllerVideoConfigSignal
  | {
      kind: "displays";
      displays: RemoteDisplaySummary[];
      activeDisplayId: number;
    }
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
    }
  | {
      kind: "clipboard";
      state: "sending" | "receiving" | "completed" | "failed";
      operation: "send" | "receive" | "paste";
      message: string;
    }
  | {
      kind: "fileTransfer";
      state: "waiting" | "sending" | "receiving" | "verifying" | "completed" | "failed" | "rejected" | "cancelled";
      direction: "upload" | "download";
      name: string;
      transferred: number;
      total: number;
      message: string;
    }
  | {
      kind: "fileQueue";
      paused: boolean;
      recoveryState: "empty" | "protected" | "memoryOnly";
      recoveryMessage: string | null;
      queued: Array<{
        id: string;
        name: string;
        size: number;
      }>;
    }
  | {
      kind: "audio";
      state: "enabled" | "muted" | "unavailable";
      enabled: boolean;
      message: string;
    }
  | {
      kind: "videoQuality";
      preference: VideoQualityPreference;
      preset: VideoQualityPreset;
    };
