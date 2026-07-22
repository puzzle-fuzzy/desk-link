import {
  cancelControllerFile,
  clearControllerFileQueue,
  chooseAndSendControllerFile,
  clearSavedDevices,
  connectDevice,
  connectSavedDevice,
  createControllerChannels,
  disconnectController,
  discardControllerFileRecovery,
  discardControllerFileQueueRecovery,
  forgetSavedDevice,
  getControllerSnapshot,
  nextControllerVideoFrame,
  reconnectController,
  removeControllerQueuedFile,
  renameSavedDevice,
  openControllerDownloadsFolder,
  pasteControllerClipboardText,
  reportControllerPlaybackPressure,
  reportControllerRenderMetrics,
  requestControllerKeyframe,
  requestControllerClipboard,
  requestControllerRemoteFile,
  queueControllerFiles,
  resumeControllerFileQueue,
  retryControllerFileQueueProtection,
  retryControllerFile,
  selectControllerDisplay,
  sendControllerInput,
  sendControllerClipboard,
  setControllerAudioEnabled,
  setControllerVideoQuality,
  sendControllerText,
} from "./api";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { ControllerChannels, ControllerVideoPayload } from "./api";
import {
  deviceCredentialsAreValid,
  formatDeviceId,
  normalizeTemporaryPassword,
} from "./device-credentials";
import { escapeHtml } from "./html";
import {
  clampWheel,
  containedPointerBounds,
  keyboardKey,
  keyboardModifierMask,
  keyboardModifiers,
  mouseButton,
  normalizedPointerPosition,
  remoteCursorContentPosition,
  type PointerBounds,
} from "./remote-input";
import type {
  ControllerInput,
  SavedControllerConnectionSummary,
  SavedDeviceCredentialSummary,
  ControllerSignal,
  ControllerSnapshot,
  ControllerVideoConfigSignal,
  RemoteDisplaySummary,
  VideoQualityPreference,
  VideoQualityPreset,
} from "./types";
import { h264CodecFromSequenceHeader, videoConfigKey } from "./video-config";
import { deviceIdsMatch, formatLastUsed } from "./saved-device";
import { RemoteInputDispatcher } from "./remote-input-dispatcher";
import { RemoteKeyboardState, type ControllerKeyInput } from "./remote-keyboard-state";
import { isRemoteClipboardPasteShortcut } from "./remote-clipboard-shortcut";
import {
  CONNECTION_PROGRESS_STEPS,
  connectionProgressPresentation,
  formatConnectionElapsed,
} from "./connection-progress";
import { icon, renderLucideIcons } from "./icons";
import { isH264Keyframe, prepareH264AccessUnit } from "./h264-annex-b";
import { RemoteAudioPlayer } from "./remote-audio";
import {
  loadRemoteScaleMode,
  normalizeRemoteScaleMode,
  saveRemoteScaleMode,
  type RemoteScaleMode,
} from "./remote-scale";
import { remotePanPosition, type RemotePanOrigin } from "./remote-pan";
import { RemoteDisplaySwitchState } from "./remote-display-switch";
import {
  appendTransferHistory,
  type TransferHistoryEntry,
} from "./file-transfer-history";
import {
  queuedFilesSummary,
  sampleTransferMetrics,
  transferMetricsLabel,
  type TransferMetrics,
} from "./file-transfer-metrics";
import {
  markTransferResultsRead,
  recordTransferResult,
  transferProgressPaintDelay,
  type TransferActivityState,
} from "./file-transfer-activity";
import {
  fileRecoveryAvailabilityAfterSignal,
  preferredDeviceIdForRecovery,
  recoveredFileTransfer,
} from "./file-transfer-recovery";
import { fileQueueProtectionPresentation } from "./file-queue-protection";
import {
  FileQueueActionGate,
  type FileQueueActionKind,
  type FileQueueActionToken,
} from "./file-queue-action";
import {
  remoteSessionSummary,
  remoteToolbarHideDelay,
  remoteToolbarVisible,
  type RemoteToolbarVisibilityInput,
} from "./remote-session-presentation";
import { VideoPlaybackPressure } from "./video-playback-pressure";
import { nextVideoPullFailureCount, SerialVideoPull } from "./video-pull-loop";

type RenderRequest = () => void;
type ControllerFeedback = { tone: "success" | "error" | "info"; message: string } | null;
type ClipboardTransferStatus = Extract<ControllerSignal, { kind: "clipboard" }>;
type FileTransferStatus = Extract<ControllerSignal, { kind: "fileTransfer" }>;
type FileQueueStatus = Omit<Extract<ControllerSignal, { kind: "fileQueue" }>, "kind">;
type AudioStatus = "starting" | "enabled" | "muted" | "unavailable";

const FRAME_PREFIX_BYTES = 17;
const VIDEO_PRESSURE_REPORT_INTERVAL_MS = 1_000;
const controllerWindow = getCurrentWindow();
let snapshot: ControllerSnapshot | null = null;
let loading = true;
let busy = false;
let cancelling = false;
let feedback: ControllerFeedback = null;
let deviceIdDraft = "";
let temporaryPasswordDraft = "";
let videoConfig: ControllerVideoConfigSignal | null = null;
let activeChannels: ControllerChannels | null = null;
let channelGeneration = 0;
let decoder: VideoDecoder | null = null;
let decoderGeneration = 0;
let decoderPreference: "hardware" | "software" = "hardware";
let decoderStallTimer: number | null = null;
let decoderRenderedBaseline = 0;
let decoderSubmittedSinceStart = 0;
let awaitingDecoderKeyframe = true;
let pendingVideoKeyframe: Uint8Array | null = null;
let pendingVideoFrame: VideoFrame | null = null;
let videoPaintFrame: number | null = null;
let receivedVideoFrames = 0;
let submittedVideoFrames = 0;
let relayCompletedFrames = 0;
let malformedVideoFrames = 0;
let decoderRecoveries = 0;
let videoPullFailures = 0;
let consecutiveDecoderStalls = 0;
let videoConfigReceivedAtMs: number | null = null;
let firstFrameMs: number | null = null;
let lastRenderMetricsReportedAtMs = 0;
let videoPlaybackPressureTimer: number | null = null;
let pointerFrame: number | null = null;
let remoteResizeObserver: ResizeObserver | null = null;
let remoteCanvasBounds: PointerBounds | null = null;
let remoteViewportBounds: DOMRectReadOnly | null = null;
let remoteScaleFrame: number | null = null;
let remoteScaleMode: RemoteScaleMode = loadRemoteScaleMode();
let remotePanMode = false;
let pointerInsideViewport = false;
let remoteCanvasElement: HTMLCanvasElement | null = null;
let remoteViewportElement: HTMLElement | null = null;
let remoteCursorElement: HTMLElement | null = null;
let fullscreenListenerInitialized = false;
let remoteFullscreenActive = false;
let remoteFullscreenBusy = false;
let remoteFullscreenDesired = false;
let remoteFullscreenOperation: Promise<void> | null = null;
let remoteFullscreenResizeTimer: number | null = null;
let remoteToolbarLastRevealedAtMs = 0;
let remoteToolbarPointerNearTop = false;
let remoteToolbarFocused = false;
let remoteToolbarTimer: number | null = null;
let requestRender: RenderRequest = () => {};
let decodedFrames = 0;
let textSending = false;
let failedVideoConfig: string | null = null;
let remoteDisplays: RemoteDisplaySummary[] = [];
let activeRemoteDisplayId: number | null = null;
const remoteDisplaySwitch = new RemoteDisplaySwitchState();
let remoteDisplaySwitchTimer: number | null = null;
let remoteDisplaySwitchStatus: {
  tone: "pending" | "success" | "error";
  message: string;
} | null = null;
let attemptStartedAtMs: number | null = null;
let attemptTarget = "";
let forgetConfirmation: string | null = null;
let renameDeviceId: string | null = null;
let renameDraft = "";
let renameBusy = false;
let transferPanelOpen = false;
let clipboardTransfer: ClipboardTransferStatus | null = null;
let fileTransfer: FileTransferStatus | null = null;
let fileRecoveryAvailable = false;
let fileQueue: FileQueueStatus = {
  queued: [],
  paused: false,
  recoveryState: "empty",
  recoveryMessage: null,
};
let transferHistory: TransferHistoryEntry[] = [];
let transferHistorySequence = 0;
let fileTransferMetrics: TransferMetrics | null = null;
let transferActivity: TransferActivityState = { unreadResults: 0, tone: null };
let transferPanelUpdateTimer: number | null = null;
let lastTransferProgressPaintAtMs: number | null = null;
let smartPasteBusy = false;
let smartPastePending = false;
let filePickerBusy = false;
let discardFileRecoveryBusy = false;
let discardFileQueueRecoveryBusy = false;
const fileQueueActions = new FileQueueActionGate();
let downloadsFolderBusy = false;
let downloadsFolderMessage = "";
let fileDropInitialized = false;
let fileDragActive = false;
let audioStatus: AudioStatus = "starting";
let audioEnabled = true;
let audioToggleBusy = false;
let audioMessage = "正在准备远端系统声音。";
let videoQualityPreference: VideoQualityPreference = "automatic";
let appliedVideoQuality: VideoQualityPreset = "sharp";
let pendingVideoQuality: VideoQualityPreference | null = null;
let videoQualityAckTimer: number | null = null;
const inputDispatcher = new RemoteInputDispatcher((input, streamId) => (
  sendControllerInput({ ...input, streamId })
));
const remoteAudio = new RemoteAudioPlayer();
const videoPlaybackPressure = new VideoPlaybackPressure();
const videoPull = new SerialVideoPull<ControllerVideoPayload>();

function resetVideoTelemetry(): void {
  videoPull.stop();
  if (videoPlaybackPressureTimer !== null) {
    window.clearTimeout(videoPlaybackPressureTimer);
    videoPlaybackPressureTimer = null;
  }
  videoPlaybackPressure.reset();
  pendingVideoKeyframe = null;
  receivedVideoFrames = 0;
  submittedVideoFrames = 0;
  relayCompletedFrames = 0;
  malformedVideoFrames = 0;
  decodedFrames = 0;
  decoderRecoveries = 0;
  videoPullFailures = 0;
  consecutiveDecoderStalls = 0;
  videoConfigReceivedAtMs = null;
  firstFrameMs = null;
  lastRenderMetricsReportedAtMs = 0;
}

export async function initializeController(renderer: RenderRequest): Promise<void> {
  requestRender = renderer;
  void initializeNativeFileDrop();
  if (!fullscreenListenerInitialized) {
    window.addEventListener("resize", scheduleRemoteFullscreenSync);
    document.addEventListener("keydown", handleRemoteFullscreenEscape, true);
    document.addEventListener("pointermove", handleRemoteToolbarPointerMove, { passive: true });
    fullscreenListenerInitialized = true;
    void synchronizeRemoteFullscreen();
  }
  try {
    snapshot = await getControllerSnapshot();
    const latestSavedDevice = snapshot.savedDevices.at(0);
    deviceIdDraft = preferredDeviceIdForRecovery(
      deviceIdDraft,
      snapshot.fileRecovery?.deviceId ?? snapshot.fileQueueRecovery?.deviceId ?? null,
      latestSavedDevice?.deviceId ?? null,
    );
    if (!fileTransfer && snapshot.fileRecovery) {
      fileTransfer = recoveredFileTransfer(snapshot.fileRecovery);
      fileRecoveryAvailable = true;
      feedback = {
        tone: "info",
        message: `${snapshot.fileRecovery.message} 目标设备：${snapshot.fileRecovery.deviceId}。恢复信息已由当前 Windows 账户加密保存。`,
      };
    } else if (snapshot.fileQueueRecovery) {
      feedback = {
        tone: "info",
        message: `${snapshot.fileQueueRecovery.message} 目标设备：${snapshot.fileQueueRecovery.deviceId}；连接后需手动点击“继续队列”。`,
      };
    } else if (snapshot.fileRecoveryError && !feedback) {
      feedback = { tone: "info", message: snapshot.fileRecoveryError };
    } else if (snapshot.fileQueueRecoveryError && !feedback) {
      feedback = { tone: "info", message: snapshot.fileQueueRecoveryError };
    }
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    loading = false;
    requestRender();
  }
}

export function prepareControllerRender(): void {
  releaseInputState();
  inputDispatcher.discardPendingMoves();
  if (pointerFrame !== null) {
    window.cancelAnimationFrame(pointerFrame);
    pointerFrame = null;
  }
  if (remoteScaleFrame !== null) {
    window.cancelAnimationFrame(remoteScaleFrame);
    remoteScaleFrame = null;
  }
  releaseVideoDecoder();
  remoteResizeObserver?.disconnect();
  remoteResizeObserver = null;
  remoteCanvasBounds = null;
  remoteViewportBounds = null;
  remoteCanvasElement = null;
  remoteViewportElement = null;
  remoteCursorElement = null;
  pointerInsideViewport = false;
  clearRemoteToolbarTimer();
  cancelScheduledTransferPanelUpdate();
}

export function renderControllerView(): string {
  if (loading) {
    return `
      <div class="controller-loading" aria-live="polite">
        ${icon("loader-circle", "controller-spinner")}
        <div><strong>正在打开控制端</strong><p>正在读取当前 Windows 账户中受保护的连接。</p></div>
      </div>
    `;
  }
  const runtime = snapshot?.runtime;
  const connected = runtime?.state === "connected";
  if (!connected) {
    remotePanMode = false;
  }
  return `
    <div class="controller-stack">
      ${feedback ? renderFeedback(feedback) : ""}
      <div class="controller-heading">
        <div>
          <h1>连接设备</h1>
          <p>输入对方电脑显示的设备 ID 和访问密码，然后在对方电脑上确认连接。</p>
        </div>
        ${renderRuntimeBadge()}
      </div>
      ${connected ? renderRemoteDesktop() : renderConnectionPanel()}
    </div>
  `;
}

export function bindControllerInteractions(): void {
  document.querySelector<HTMLButtonElement>("[data-controller-dismiss]")?.addEventListener("click", () => {
    feedback = null;
    requestRender();
  });
  document
    .querySelector<HTMLFormElement>("[data-controller-device-form]")
    ?.addEventListener("submit", (event) => void submitDevice(event));
  document.querySelector<HTMLInputElement>("[data-controller-device-id]")?.addEventListener("input", (event) => {
    const input = event.currentTarget as HTMLInputElement;
    deviceIdDraft = formatDeviceId(input.value);
    input.value = deviceIdDraft;
    updateDeviceSubmitState();
  });
  document.querySelector<HTMLInputElement>("[data-controller-password]")?.addEventListener("input", (event) => {
    const input = event.currentTarget as HTMLInputElement;
    temporaryPasswordDraft = normalizeTemporaryPassword(input.value);
    input.value = temporaryPasswordDraft;
    updateDeviceSubmitState();
  });
  document.querySelectorAll<HTMLButtonElement>("[data-controller-saved-device-connect]").forEach((button) => {
    button.addEventListener("click", () => {
      const selected = button.dataset.controllerSavedDeviceConnect;
      if (!selected) {
        return;
      }
      void beginStoredDeviceConnection(selected);
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-controller-saved-device-update]").forEach((button) => {
    button.addEventListener("click", () => {
      const selected = button.dataset.controllerSavedDeviceUpdate;
      if (!selected) {
        return;
      }
      forgetConfirmation = null;
      deviceIdDraft = formatDeviceId(selected);
      temporaryPasswordDraft = "";
      feedback = { tone: "info", message: "请输入主机当前显示的新密码；验证成功后会替换已保存密码。" };
      requestRender();
      window.setTimeout(() => document.querySelector<HTMLInputElement>("[data-controller-password]")?.focus(), 0);
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-controller-saved-device-rename]").forEach((button) => {
    button.addEventListener("click", () => {
      const selected = button.dataset.controllerSavedDeviceRename;
      const saved = snapshot?.savedDevices.find((device) => deviceIdsMatch(device.deviceId, selected ?? ""));
      if (!selected || !saved) {
        return;
      }
      forgetConfirmation = null;
      renameDeviceId = selected;
      renameDraft = saved.alias ?? "";
      requestRender();
      window.setTimeout(
        () => document.querySelector<HTMLInputElement>("[data-controller-device-alias]")?.focus(),
        0,
      );
    });
  });
  document.querySelector<HTMLInputElement>("[data-controller-device-alias]")?.addEventListener("input", (event) => {
    renameDraft = (event.currentTarget as HTMLInputElement).value;
  });
  document.querySelector<HTMLFormElement>("[data-controller-device-rename-form]")?.addEventListener("submit", (event) => {
    void submitDeviceRename(event);
  });
  document.querySelector<HTMLButtonElement>("[data-controller-cancel-rename]")?.addEventListener("click", () => {
    clearRenameDraft();
    requestRender();
  });
  document.querySelectorAll<HTMLButtonElement>("[data-controller-request-forget]").forEach((button) => {
    button.addEventListener("click", () => {
      const selected = button.dataset.controllerRequestForget;
      if (selected) {
        clearRenameDraft();
        forgetConfirmation = selected;
        requestRender();
        window.setTimeout(
          () => document.querySelector<HTMLButtonElement>("[data-controller-confirm-forget]")?.focus(),
          0,
        );
      }
    });
  });
  document.querySelector<HTMLButtonElement>("[data-controller-cancel-forget]")?.addEventListener("click", () => {
    forgetConfirmation = null;
    requestRender();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-confirm-forget]")?.addEventListener("click", () => {
    if (forgetConfirmation) {
      void removeStoredDevice(forgetConfirmation);
    }
  });
  document.querySelector<HTMLButtonElement>("[data-controller-saved-devices-clear]")?.addEventListener("click", () => {
    void clearStoredDevices();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-reconnect]")?.addEventListener("click", () => {
    void beginSavedConnection();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-disconnect]")?.addEventListener("click", () => {
    void cancelConnection();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-cancel]")?.addEventListener("click", () => {
    void cancelConnection();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-keyframe]")?.addEventListener("click", () => {
    retryVideo();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-audio]")?.addEventListener("click", () => {
    void toggleRemoteAudio();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-fullscreen]")?.addEventListener("click", () => {
    void toggleFullscreen();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-transfer]")?.addEventListener("click", () => {
    transferPanelOpen = !transferPanelOpen;
    if (transferPanelOpen) {
      transferActivity = markTransferResultsRead(transferActivity);
    } else {
      cancelScheduledTransferPanelUpdate();
    }
    updateTransferPanel({ ledger: true });
    revealRemoteToolbar();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-clipboard-send]")?.addEventListener("click", () => {
    void sendLocalClipboard();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-clipboard-request]")?.addEventListener("click", () => {
    void receiveRemoteClipboard();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-file-send]")?.addEventListener("click", () => {
    void chooseFileForTransfer();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-file-receive]")?.addEventListener("click", () => {
    void requestRemoteFileTransfer();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-file-cancel]")?.addEventListener("click", () => {
    void cancelFileTransfer();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-file-retry]")?.addEventListener("click", () => {
    void retryFileTransfer();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-file-discard]")?.addEventListener("click", () => {
    void discardFileRecovery();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-downloads-open]")?.addEventListener("click", () => {
    void openDownloadsFolder();
  });
  document.querySelector<HTMLSelectElement>("[data-controller-display]")?.addEventListener("change", (event) => {
    void changeRemoteDisplay(Number((event.currentTarget as HTMLSelectElement).value));
  });
  document.querySelector<HTMLSelectElement>("[data-controller-video-quality]")?.addEventListener("change", (event) => {
    void changeVideoQuality((event.currentTarget as HTMLSelectElement).value as VideoQualityPreference);
  });
  document.querySelector<HTMLSelectElement>("[data-controller-scale]")?.addEventListener("change", (event) => {
    changeRemoteScaleMode((event.currentTarget as HTMLSelectElement).value);
  });
  document.querySelector<HTMLButtonElement>("[data-controller-pan]")?.addEventListener("click", () => {
    setRemotePanMode(!remotePanMode);
  });
  document.querySelector<HTMLButtonElement>("[data-controller-windows-key]")?.addEventListener("click", () => {
    sendRemoteKeyTap("meta");
  });
  document.querySelector<HTMLButtonElement>("[data-controller-text]")?.addEventListener("click", () => {
    const panel = document.querySelector<HTMLElement>("[data-controller-text-panel]");
    const input = document.querySelector<HTMLInputElement>("[data-controller-text-input]");
    if (panel && input) {
      panel.hidden = false;
      revealRemoteToolbar();
      input.focus();
    }
  });
  document.querySelector<HTMLButtonElement>("[data-controller-text-cancel]")?.addEventListener("click", () => {
    closeTextInput();
  });
  document.querySelector<HTMLFormElement>("[data-controller-text-form]")?.addEventListener("submit", (event) => {
    void submitTextInput(event);
  });
  bindTransferLedgerActions();
  const remoteToolbar = document.querySelector<HTMLElement>(".remote-toolbar");
  remoteToolbar?.addEventListener("pointerenter", () => {
    remoteToolbarPointerNearTop = true;
    revealRemoteToolbar();
  });
  remoteToolbar?.addEventListener("pointerleave", (event) => {
    remoteToolbarPointerNearTop = event.clientY <= 12;
    revealRemoteToolbar();
  });
  remoteToolbar?.addEventListener("focusin", () => {
    remoteToolbarFocused = true;
    revealRemoteToolbar();
  });
  remoteToolbar?.addEventListener("focusout", () => {
    queueMicrotask(() => {
      remoteToolbarFocused = remoteToolbar.contains(document.activeElement);
      revealRemoteToolbar();
    });
  });
  if (snapshot?.runtime.state === "connected") {
    setupRemoteDesktop();
    syncRemoteInteractionControls();
    syncRemoteFullscreenControl();
    updateRemoteToolbarVisibility();
  }
}

function renderFeedback(item: NonNullable<ControllerFeedback>): string {
  return `
    <div class="feedback feedback--${item.tone}" role="${item.tone === "error" ? "alert" : "status"}" aria-live="${item.tone === "error" ? "assertive" : "polite"}">
      ${icon(item.tone === "success" ? "circle-check" : item.tone === "error" ? "circle-alert" : "info", "feedback-symbol")}
      <span>${escapeHtml(item.message)}</span>
      <button type="button" class="feedback-close" data-controller-dismiss aria-label="关闭消息">${icon("x")}</button>
    </div>
  `;
}

function renderRuntimeBadge(): string {
  const runtime = snapshot?.runtime;
  const state = runtime?.state ?? "idle";
  const label = runtime?.title ?? "控制端不可用";
  return `
    <div class="controller-runtime controller-runtime--${state}">
      <span aria-hidden="true"></span>
      <div><strong>${escapeHtml(label)}</strong><small>${escapeHtml(runtime?.detail ?? "请重新打开 DeskLink。")}</small></div>
    </div>
  `;
}

function renderConnectionPanel(): string {
  const connectionState = snapshot?.runtime.state ?? "idle";
  const connectionActive = busy || isActiveConnectionState(connectionState);
  const isWorking =
    connectionActive
    || cancelling
    || renameBusy
    ;
  const credentialsReady = deviceCredentialsAreValid(deviceIdDraft, temporaryPasswordDraft);
  const retryAvailable = feedback?.tone === "error" && credentialsReady;
  const hasQuickConnections = Boolean(snapshot?.savedConnection) || (snapshot?.savedDevices.length ?? 0) > 0;
  return `
    ${connectionActive || cancelling ? renderConnectionProgress(connectionState) : ""}
    <div class="controller-connect-layout">
      <div class="controller-connect-column controller-connect-column--new">
        <section class="controller-card controller-card--primary controller-card--manual">
          <div class="controller-card-heading">
            <div><h2>${hasQuickConnections ? "连接新的设备" : "连接远程设备"}</h2><p>输入另一台电脑显示的本机 ID，以及临时密码或固定密码。</p></div>
          </div>
          <form class="controller-form controller-device-form" data-controller-device-form>
            <label class="field device-credential-field">
              <span>设备 ID</span>
              <input class="device-id-input" name="deviceId" data-controller-device-id value="${escapeHtml(deviceIdDraft)}" inputmode="numeric" maxlength="15" placeholder="123 456 789 012" aria-describedby="controller-device-hint" required autocomplete="off" spellcheck="false" ${isWorking ? "disabled" : ""}>
            </label>
            <label class="field device-credential-field">
              <span>访问密码</span>
              <input class="temporary-password-input" name="temporaryPassword" data-controller-password value="${escapeHtml(temporaryPasswordDraft)}" maxlength="8" placeholder="8 位访问密码" aria-describedby="controller-device-hint" required autocomplete="one-time-code" autocapitalize="characters" spellcheck="false" ${isWorking ? "disabled" : ""}>
            </label>
            <p class="controller-device-hint" id="controller-device-hint">验证成功后，设备 ID 和密码会由当前 Windows 账户加密保存。</p>
            <div class="controller-form-actions">
              <button class="button button--primary" type="submit" data-controller-device-submit ${isWorking || !credentialsReady ? "disabled" : ""} ${isWorking ? 'aria-busy="true"' : ""}>
                ${connectionActive ? `${icon("loader-circle", "button-spinner")} ${escapeHtml(connectionActionLabel(connectionState))}` : connectionState === "stopped" || retryAvailable ? "重新尝试" : "查找并连接设备"}
              </button>
              <span>${connectionActive ? "取消后仍会保留当前输入。" : "找到设备后，需要在目标电脑上确认一次。"}</span>
            </div>
          </form>
          <div class="controller-privacy-note" title="远程画面和输入经过端到端加密，新控制端必须在主机上获得本地批准。">
            ${icon("shield-check")}<span>首次连接需主机确认，画面与输入端到端加密</span>
          </div>
        </section>
      </div>
      ${renderSavedDevices(isWorking)}
    </div>
  `;
}

function renderConnectionProgress(state: string): string {
  const runtimeState = snapshot?.runtime.state ?? "finding";
  const progressState = isActiveConnectionState(runtimeState) ? runtimeState : "finding";
  const elapsedSeconds = connectionElapsedSeconds();
  const presentation = connectionProgressPresentation(progressState, elapsedSeconds);
  const target = attemptTarget || snapshot?.savedConnection?.deviceId || "当前设备";
  const title = isActiveConnectionState(runtimeState)
    ? snapshot?.runtime.title
    : cancelling
      ? "正在取消连接"
      : "正在开始连接";
  const detail = isActiveConnectionState(runtimeState)
    ? snapshot?.runtime.detail
    : cancelling
      ? "DeskLink 正在停止本次连接并释放远程会话。"
      : "DeskLink 正在准备设备查询和加密通道。";
  return `
    <section class="connection-progress" aria-labelledby="connection-progress-heading" aria-live="polite">
      <div class="connection-progress-heading">
        <div>
          <span class="connection-progress-kicker">正在连接 ${escapeHtml(target)}</span>
          <h2 id="connection-progress-heading">${escapeHtml(title ?? connectionActionLabel(state))}</h2>
          <p>${escapeHtml(detail ?? "DeskLink 正在准备连接。")}</p>
        </div>
        <span class="connection-progress-time" data-controller-attempt-elapsed>${formatConnectionElapsed(elapsedSeconds)}</span>
      </div>
      <ol class="connection-progress-steps" aria-label="远程连接进度">
        ${CONNECTION_PROGRESS_STEPS.map((label, index) => {
          const stepState = index < presentation.activeStep
            ? "complete"
            : index === presentation.activeStep
              ? "active"
              : "pending";
          return `<li class="connection-progress-step connection-progress-step--${stepState}" ${stepState === "active" ? 'aria-current="step"' : ""}>
            <span class="connection-progress-index" aria-hidden="true">${stepState === "complete" ? icon("check") : index + 1}</span>
            <span>${label}</span>
          </li>`;
        }).join("")}
      </ol>
      <div class="connection-progress-footer">
        <p class="connection-progress-guidance ${presentation.delayed ? "connection-progress-guidance--delayed" : ""}" data-controller-attempt-guidance>${escapeHtml(presentation.guidance)}</p>
        <button class="button button--secondary" type="button" data-controller-cancel ${cancelling ? "disabled" : ""}>${cancelling ? "正在取消…" : "取消连接"}</button>
      </div>
    </section>
  `;
}

function renderSavedDevices(isWorking: boolean): string {
  const savedDevices = snapshot?.savedDevices ?? [];
  const approvedConnection = snapshot?.savedConnection ?? null;
  const approvedListed = approvedConnection
    ? savedDevices.some((saved) => deviceIdsMatch(saved.deviceId, approvedConnection.deviceId))
    : false;
  const hasSavedDevice = savedDevices.length > 0 || Boolean(approvedConnection);
  return `
    <aside class="saved-devices-panel" aria-label="可直接连接的设备">
      <div class="saved-devices-heading">
        <div><h2>已保存设备</h2><p>选择使用过的电脑，直接开始连接。</p></div>
        ${snapshot?.savedDevicesError ? `<button type="button" data-controller-saved-devices-clear ${isWorking ? "disabled" : ""}>清除异常记录</button>` : ""}
      </div>
      ${snapshot?.savedDevicesError ? `<p class="inline-error">${escapeHtml(snapshot.savedDevicesError)}</p>` : ""}
      ${snapshot?.connectionError ? `<p class="inline-error">${escapeHtml(snapshot.connectionError)}</p>` : ""}
      <div class="saved-device-list">
        ${hasSavedDevice
          ? `${savedDevices.map((saved) => renderSavedDevice(saved, approvedConnection, isWorking)).join("")}
             ${approvedConnection && !approvedListed ? renderApprovedConnection(approvedConnection, isWorking) : ""}`
          : `<div class="saved-devices-empty">${icon("monitor-up")}<strong>还没有已保存设备</strong><p>成功连接后，设备会显示在这里，方便下次直接连接。</p></div>`}
      </div>
    </aside>
  `;
}

function renderSavedDevice(
  saved: SavedDeviceCredentialSummary,
  approvedConnection: SavedControllerConnectionSummary | null,
  isWorking: boolean,
): string {
  const approved = approvedConnection ? deviceIdsMatch(saved.deviceId, approvedConnection.deviceId) : false;
  const kind = approved ? "approved" : saved.persistent ? "fixed" : "temporary";
  const label = approved ? "已批准" : saved.persistent ? "固定密码" : "临时密码";
  const detail = approved
    ? "已保存安全连接，可直接重新连接。"
    : saved.persistent
      ? "固定密码已加密保存，可直接查找主机。"
      : "临时密码可能过期，失败时请更新密码。";
  const displayName = saved.alias || saved.deviceId;
  const idDetail = saved.alias
    ? `<span class="saved-device-public-id">${escapeHtml(saved.deviceId)}</span><span aria-hidden="true"> · </span>`
    : "";
  return `
    <article class="saved-device-row">
      <div class="saved-device-identity">
        <strong>${escapeHtml(displayName)}</strong>
        <span class="saved-device-kind saved-device-kind--${kind}">${label}</span>
        <small>${idDetail}${detail} 最近使用：${escapeHtml(formatLastUsed(saved.lastUsedUnixS))}</small>
      </div>
      <div class="saved-device-actions">
        <button class="button button--primary" type="button" ${approved ? "data-controller-reconnect" : `data-controller-saved-device-connect="${escapeHtml(saved.deviceId)}"`} ${isWorking ? "disabled" : ""}>直接连接</button>
        <button class="button button--secondary" type="button" data-controller-saved-device-rename="${escapeHtml(saved.deviceId)}" ${isWorking ? "disabled" : ""}>修改名称</button>
        <button class="button button--secondary" type="button" data-controller-saved-device-update="${escapeHtml(saved.deviceId)}" ${isWorking ? "disabled" : ""}>更新密码</button>
        <button class="saved-device-remove" type="button" data-controller-request-forget="${escapeHtml(saved.deviceId)}" ${isWorking ? "disabled" : ""}>移除记录</button>
      </div>
      ${renderRenameEditor(saved)}
      ${renderForgetConfirmation(saved.deviceId)}
    </article>
  `;
}

function renderRenameEditor(saved: SavedDeviceCredentialSummary): string {
  if (!renameDeviceId || !deviceIdsMatch(renameDeviceId, saved.deviceId)) {
    return "";
  }
  return `
    <form class="saved-device-rename" data-controller-device-rename-form>
      <label for="saved-device-alias"><span>设备名称</span><small>仅保存在当前 Windows 账户中，不会发送给远程电脑。</small></label>
      <input id="saved-device-alias" name="alias" data-controller-device-alias value="${escapeHtml(renameDraft)}" maxlength="32" placeholder="例如：办公室电脑" autocomplete="off" spellcheck="false" ${renameBusy ? "disabled" : ""}>
      <button class="button button--secondary" type="button" data-controller-cancel-rename ${renameBusy ? "disabled" : ""}>取消修改</button>
      <button class="button button--primary" type="submit" ${renameBusy ? "disabled" : ""} ${renameBusy ? 'aria-busy="true"' : ""}>${renameBusy ? "正在保存…" : renameDraft.trim() ? "保存名称" : "清除名称"}</button>
    </form>
  `;
}

function renderApprovedConnection(saved: SavedControllerConnectionSummary, isWorking: boolean): string {
  return `
    <article class="saved-device-row">
      <div class="saved-device-identity">
        <strong>${escapeHtml(saved.deviceId)}</strong>
        <span class="saved-device-kind saved-device-kind--approved">已批准</span>
        <small>已保存端到端安全连接，可直接重新连接。</small>
      </div>
      <div class="saved-device-actions">
        <button class="button button--primary" type="button" data-controller-reconnect ${isWorking ? "disabled" : ""}>直接连接</button>
        <button class="saved-device-remove" type="button" data-controller-request-forget="${escapeHtml(saved.deviceId)}" ${isWorking ? "disabled" : ""}>移除记录</button>
      </div>
      ${renderForgetConfirmation(saved.deviceId)}
    </article>
  `;
}

function renderForgetConfirmation(deviceId: string): string {
  if (!forgetConfirmation || !deviceIdsMatch(forgetConfirmation, deviceId)) {
    return "";
  }
  return `
    <div class="saved-device-confirm" role="group" aria-label="确认移除设备 ${escapeHtml(deviceId)}">
      <div><strong>移除此设备记录？</strong><span>下次连接需要重新输入设备 ID 和访问密码。</span></div>
      <button class="button button--secondary" type="button" data-controller-cancel-forget>保留记录</button>
      <button class="button button--danger-quiet" type="button" data-controller-confirm-forget>移除记录</button>
    </div>
  `;
}

function renderTransferToolbarButton(): string {
  const activity = transferToolbarActivityView();
  return `<button class="toolbar-button${activity.visible ? " toolbar-button--has-activity" : ""}" type="button" data-controller-transfer aria-expanded="${transferPanelOpen}" title="${activity.title}">${icon("share-2")}<span>传输</span><span class="remote-transfer-activity" data-controller-transfer-activity data-state="${activity.tone}" ${activity.visible ? "" : "hidden"} role="status" aria-live="polite" aria-label="${activity.ariaLabel}">${activity.badge}</span></button>`;
}

function transferToolbarActivityView(): {
  visible: boolean;
  tone: "active" | "success" | "error" | "idle";
  badge: string;
  title: string;
  ariaLabel: string;
} {
  const active = isFileTransferActive(fileTransfer?.state);
  const unread = transferActivity.unreadResults;
  if (active) {
    const resultText = unread > 0 ? `，另有 ${unread} 条未查看结果` : "";
    return {
      visible: true,
      tone: "active",
      badge: unread > 0 ? String(unread) : "",
      title: `文件正在传输${resultText}`,
      ariaLabel: `文件正在传输${resultText}`,
    };
  }
  if (unread > 0) {
    return {
      visible: true,
      tone: transferActivity.tone ?? "success",
      badge: String(unread),
      title: `${unread} 条文件传输结果未查看`,
      ariaLabel: `${unread} 条文件传输结果未查看`,
    };
  }
  return {
    visible: false,
    tone: "idle",
    badge: "",
    title: "发送剪贴板文本或文件",
    ariaLabel: "",
  };
}

function renderRemoteDesktop(): string {
  const config = videoConfig;
  const videoFailed = config ? failedVideoConfig === videoConfigKey(config) : false;
  return `
    <section class="remote-session" data-remote-toolbar-visible="true" aria-label="当前远程控制会话">
      <div class="remote-toolbar">
        <div class="remote-toolbar-status">
          <span class="remote-live-dot" aria-hidden="true"></span>
          <div><strong>实时远程桌面</strong><small data-controller-metrics>${config ? remoteSessionSummary(config.width, config.height, videoQualityPreference, appliedVideoQuality) : "正在等待首个视频画面"}</small></div>
        </div>
        <div class="remote-toolbar-actions">
          <label class="remote-quality-picker" title="根据当前网络调整远程画面的清晰度和流畅度">
            ${icon("gauge")}
            <span class="sr-only">选择远程画质</span>
            <select data-controller-video-quality aria-label="选择远程画质" ${pendingVideoQuality === null ? "" : "disabled"}>
              <option value="automatic" ${videoQualityPreference === "automatic" ? "selected" : ""}>自动（${videoQualityPresetLabel(appliedVideoQuality)}）</option>
              <option value="smooth" ${videoQualityPreference === "smooth" ? "selected" : ""}>流畅</option>
              <option value="balanced" ${videoQualityPreference === "balanced" ? "selected" : ""}>均衡</option>
              <option value="sharp" ${videoQualityPreference === "sharp" ? "selected" : ""}>清晰</option>
            </select>
          </label>
          ${renderAudioControl()}
          ${remoteDisplays.length > 1 ? `
            <label class="remote-display-picker" title="切换目标电脑的显示器">
              ${icon("monitor")}
              <span class="sr-only">选择远程显示器</span>
              <select data-controller-display aria-label="选择远程显示器" ${remoteDisplaySwitch.pendingId === null ? "" : "disabled"}>
                ${remoteDisplays.map((display, index) => `<option value="${display.id}" ${display.id === (remoteDisplaySwitch.pendingId ?? activeRemoteDisplayId) ? "selected" : ""}>屏幕 ${index + 1}${display.primary ? "（主屏）" : ""} · ${display.width} × ${display.height}</option>`).join("")}
              </select>
            </label><span class="remote-display-switch-status" data-controller-display-status data-tone="${remoteDisplaySwitchStatus?.tone ?? ""}" ${remoteDisplaySwitchStatus ? "" : "hidden"} role="status" aria-live="polite">${escapeHtml(remoteDisplaySwitchStatus?.message ?? "")}</span>` : ""}
          <label class="remote-scale-picker" title="适应窗口会显示完整画面；1:1 会按收到的画面像素显示，超出区域可用滚动条查看">
            ${icon("scan")}
            <span class="sr-only">远程画面缩放</span>
            <select data-controller-scale aria-label="远程画面缩放">
              <option value="fit" ${remoteScaleMode === "fit" ? "selected" : ""}>适应</option>
              <option value="actual" ${remoteScaleMode === "actual" ? "selected" : ""}>1:1</option>
            </select>
          </label>
          <button class="toolbar-button${remotePanMode ? " toolbar-button--active" : ""}" type="button" data-controller-pan aria-pressed="${remotePanMode}" ${remoteScaleMode === "actual" ? "" : "disabled"} title="${remoteScaleMode === "actual" ? (remotePanMode ? "恢复向远程电脑发送鼠标和键盘输入" : "只在本地拖动或滚动画面，不会操作远程电脑") : "切换到 1:1 后可以浏览超出窗口的画面"}">${icon("hand")}${remotePanMode ? "继续控制" : "浏览画面"}</button>
          <button class="toolbar-button" type="button" data-controller-windows-key title="在远程电脑打开或关闭开始菜单，不会打开本机开始菜单">${icon("panels-top-left")}Windows 键</button>
          <button class="toolbar-button" type="button" data-controller-text title="发送中文、符号或一段文字">${icon("keyboard")}发送文字</button>
          ${renderTransferToolbarButton()}
          <button class="toolbar-button" type="button" data-controller-keyframe title="刷新远程画面">${icon("refresh-cw")}刷新画面</button>
          <button class="toolbar-button" type="button" data-controller-fullscreen aria-label="${remoteFullscreenActive ? "退出全屏" : "进入全屏"}" aria-pressed="${remoteFullscreenActive}" ${remoteFullscreenBusy ? 'disabled aria-busy="true"' : ""}>${icon(remoteFullscreenActive ? "minimize-2" : "maximize-2")}<span data-controller-fullscreen-label>${remoteFullscreenActive ? "退出全屏" : "全屏"}</span></button>
          <button class="toolbar-button toolbar-button--danger" type="button" data-controller-disconnect>${icon("log-out")}断开连接</button>
        </div>
      </div>
      <div class="remote-fullscreen-guide" aria-hidden="true">鼠标移到顶部显示工具栏 · Esc 退出全屏</div>
      ${renderTransferPanel()}
      <form class="remote-text-entry" data-controller-text-form data-controller-text-panel hidden>
        <label for="remote-text-input">发送文字到远程电脑</label>
        <input id="remote-text-input" data-controller-text-input type="text" maxlength="256" autocomplete="off" placeholder="可输入或粘贴中文、符号和短文本" required>
        <button class="toolbar-button" type="submit">${icon("send-horizontal")}发送文字</button>
        <button class="toolbar-button" type="button" data-controller-text-cancel>取消</button>
      </form>
      <div class="remote-viewport" data-remote-viewport data-scale-mode="${remoteScaleMode}" data-interaction-mode="${remotePanMode ? "pan" : "control"}" tabindex="0" aria-label="远程 Windows 桌面，点击后可发送键盘和鼠标输入。">
        ${videoFailed
          ? '<div class="remote-waiting remote-waiting--error"><strong>远程画面暂时无法解码</strong><p>请更新 WebView2，或点击“刷新画面”再试一次。</p></div>'
          : config
            ? `<canvas class="remote-canvas" data-remote-canvas width="${config.width}" height="${config.height}"></canvas><div class="remote-video-starting" data-remote-video-starting>${icon("loader-circle", "controller-spinner")}<strong>正在启动远程画面</strong><p>正在接收并解码第一个加密视频帧。</p></div><span class="remote-cursor" data-remote-cursor aria-hidden="true" hidden>${icon("mouse-pointer-2")}</span>`
            : `<div class="remote-waiting">${icon("loader-circle", "controller-spinner")}<strong>正在准备远程画面</strong><p>DeskLink 协商视频流时，请保持此窗口打开。</p></div>`}
        <div class="remote-focus-hint" data-remote-focus-hint>${remotePanMode ? "浏览模式：拖动画面或滚轮查看，不会操作远程电脑" : "点击画面开始控制 · Ctrl+V 粘贴本机短文本 · Ctrl+Alt+Delete 必须在主机本地操作"}</div>
        <div class="remote-file-drop-overlay" data-controller-file-drop-overlay ${fileDragActive ? "" : "hidden"} aria-hidden="true">
          ${icon("file-up")}<strong>松开鼠标，将文件加入发送队列</strong><span>文件会按顺序发送，不会打断画面和输入。</span>
        </div>
      </div>
    </section>
  `;
}

function renderAudioControl(): string {
  const unavailable = audioStatus === "starting" || audioStatus === "unavailable";
  const muted = audioStatus === "muted" || !audioEnabled;
  const iconName = unavailable ? "volume-off" : muted ? "volume-x" : "volume-2";
  const label = audioStatus === "starting"
    ? "声音准备中"
    : audioStatus === "unavailable"
      ? "无声音"
      : muted
        ? "打开声音"
        : "静音";
  return '<button class="toolbar-button" type="button" data-controller-audio aria-pressed="'
    + String(muted)
    + '" title="'
    + escapeHtml(audioMessage)
    + '"'
    + (unavailable || audioToggleBusy ? " disabled" : "")
    + ">"
    + icon(iconName)
    + '<span data-controller-audio-label>'
    + label
    + "</span></button>";
}

function renderTransferPanel(): string {
  const fileActive = isFileTransferActive(fileTransfer?.state);
  const clipboardActive = isClipboardTransferActive(clipboardTransfer?.state);
  const fileRetryable = fileRecoveryAvailable && isFileTransferRetryable(fileTransfer?.state);
  const progress = transferPercent(fileTransfer?.transferred ?? 0, fileTransfer?.total ?? 0);
  const details = fileTransfer ? formatTransferDetails(fileTransfer) : "";
  const receivedFile = hasCompletedDownload();
  return `
    <section class="remote-transfer-panel" data-controller-transfer-panel ${transferPanelOpen ? "" : "hidden"} aria-label="剪贴板与文件传输">
      <div class="remote-transfer-heading">
        <div>${icon("share-2")}<span><strong>剪贴板与文件</strong><small>可双向传输；获取文件时由远端电脑选择。</small></span></div>
        <div class="remote-transfer-actions">
          <button class="toolbar-button" type="button" data-controller-clipboard-send ${clipboardActive ? "disabled" : ""}>${icon("clipboard-copy")}发送本机剪贴板</button>
          <button class="toolbar-button" type="button" data-controller-clipboard-request ${clipboardActive ? "disabled" : ""}>${icon("clipboard-paste")}读取远端剪贴板</button>
          <button class="toolbar-button" type="button" data-controller-file-send ${filePickerBusy || discardFileRecoveryBusy || discardFileQueueRecoveryBusy || fileQueueActions.busy ? "disabled" : ""}>${icon("file-up")}${filePickerBusy ? "正在选择…" : "添加发送文件"}</button>
          <button class="toolbar-button" type="button" data-controller-file-receive ${fileActive || fileQueue.queued.length > 0 || discardFileRecoveryBusy || discardFileQueueRecoveryBusy || fileQueueActions.busy ? "disabled" : ""}>${icon("file-down")}获取远端文件</button>
        </div>
      </div>
      <div class="remote-file-drop-hint">
        <span>${icon("file-up")}也可以把一个或多个文件拖到远程画面，最多排队 20 个。</span>
        <span class="remote-downloads-action" ${receivedFile ? "" : "hidden"}>
          <button class="toolbar-button toolbar-button--quiet" type="button" data-controller-downloads-open ${downloadsFolderBusy ? "disabled" : ""}>${icon("folder-open")}${downloadsFolderBusy ? "正在打开…" : "打开下载文件夹"}</button>
          <small data-controller-downloads-message aria-live="polite">${escapeHtml(downloadsFolderMessage)}</small>
        </span>
      </div>
      <div class="remote-transfer-statuses">
        <div class="remote-transfer-status" data-controller-clipboard-status ${clipboardTransfer ? "" : "hidden"}>
          <strong>剪贴板</strong><span data-controller-clipboard-message>${escapeHtml(clipboardTransfer?.message ?? "")}</span>
        </div>
        <div class="remote-transfer-status remote-transfer-status--file" data-controller-file-status ${fileTransfer ? "" : "hidden"}>
          <div class="remote-transfer-file-copy"><strong data-controller-file-name>${escapeHtml(fileTransfer?.name ?? "")}</strong><span data-controller-file-message>${escapeHtml(fileTransfer?.message ?? "")}</span><small data-controller-file-size>${escapeHtml(details)}</small></div>
          <progress data-controller-file-progress max="100" value="${progress}" aria-label="文件传输进度"></progress>
          <div class="remote-transfer-file-actions">
            <button class="toolbar-button toolbar-button--quiet" type="button" data-controller-file-retry ${fileRetryable ? "" : "hidden"} ${discardFileRecoveryBusy ? "disabled" : ""}>${icon("rotate-ccw")}${fileTransfer?.direction === "download" ? "重新获取" : "重新发送"}</button>
            <button class="toolbar-button toolbar-button--quiet" type="button" data-controller-file-discard ${fileRetryable ? "" : "hidden"} ${discardFileRecoveryBusy ? "disabled" : ""}>${icon("trash-2")}${discardFileRecoveryBusy ? "正在清理…" : "不再重试"}</button>
            <button class="toolbar-button toolbar-button--quiet" type="button" data-controller-file-cancel ${fileActive ? "" : "hidden"}>取消</button>
          </div>
        </div>
      </div>
      ${renderFileQueueRecovery()}
      <div class="remote-transfer-ledger">
        <section class="remote-transfer-list" data-controller-file-queue ${fileQueue.queued.length > 0 ? "" : "hidden"} aria-label="等待发送的文件">
          ${renderFileQueue()}
        </section>
        <section class="remote-transfer-list" data-controller-file-history ${transferHistory.length > 0 ? "" : "hidden"} aria-label="最近传输记录">
          ${renderTransferHistory()}
        </section>
      </div>
    </section>
  `;
}

function renderFileQueueRecovery(): string {
  const recovery = snapshot?.fileQueueRecovery;
  const protectionFailure = fileQueue.queued.length === 0
    && fileQueue.recoveryState === "memoryOnly"
    && fileQueue.recoveryMessage;
  if (protectionFailure) {
    const retrying = fileQueueActions.matches("protect");
    return `
      <section class="remote-queue-recovery" data-controller-file-queue-recovery aria-label="等待队列保护失败">
        <span class="remote-queue-recovery__icon">${icon("shield-alert")}</span>
        <span class="remote-queue-recovery__copy"><strong>等待队列状态尚未安全更新</strong><small>${escapeHtml(protectionFailure)}</small></span>
        <button class="toolbar-button toolbar-button--quiet" type="button" data-controller-file-queue-protection-retry ${fileQueueActions.busy ? "disabled" : ""}>${icon(retrying ? "loader-circle" : "rotate-ccw", retrying ? "button-spinner" : "")}${retrying ? "正在重试…" : "重试保护"}</button>
      </section>
    `;
  }
  if (!recovery || fileQueue.queued.length > 0) {
    return '<section class="remote-queue-recovery" data-controller-file-queue-recovery hidden></section>';
  }
  const targetMatches = deviceIdsMatch(recovery.deviceId, attemptTarget || deviceIdDraft);
  return `
    <section class="remote-queue-recovery" data-controller-file-queue-recovery aria-label="等待队列恢复">
      <span class="remote-queue-recovery__icon">${icon("rotate-ccw")}</span>
      <span class="remote-queue-recovery__copy">
        <strong>还有 ${recovery.queued.length} 个文件等待发送</strong>
        <small>${targetMatches
          ? `队列属于当前设备 ${escapeHtml(recovery.deviceId)}；载入后仍会保持暂停，请点击“继续队列”。`
          : `队列属于设备 ${escapeHtml(recovery.deviceId)}，不会发送到当前电脑。`}</small>
      </span>
      <button class="toolbar-button toolbar-button--quiet" type="button" data-controller-file-queue-recovery-discard ${discardFileQueueRecoveryBusy ? "disabled" : ""}>${icon("trash-2")}${discardFileQueueRecoveryBusy ? "正在清理…" : "放弃旧队列"}</button>
    </section>
  `;
}

function renderFileQueue(): string {
  const visible = fileQueue.queued.slice(0, 4);
  const summary = queuedFilesSummary(fileQueue.queued);
  const queueActionsDisabled = fileQueueActions.busy || filePickerBusy || discardFileQueueRecoveryBusy;
  const resuming = fileQueueActions.matches("resume");
  const clearing = fileQueueActions.matches("clear");
  return `
    <div class="remote-transfer-list-heading"><strong>${fileQueue.paused ? "队列已暂停" : "等待发送"} · ${summary}</strong><span>${fileQueue.paused ? `<button type="button" data-controller-file-queue-resume ${queueActionsDisabled ? "disabled" : ""}>${resuming ? `${icon("loader-circle", "button-spinner")}正在继续…` : "继续队列"}</button>` : ""}<button type="button" data-controller-file-queue-clear ${queueActionsDisabled ? "disabled" : ""}>${clearing ? `${icon("loader-circle", "button-spinner")}正在清空…` : "清空等待"}</button></span></div>
    ${renderFileQueueProtection()}
    <ul>${visible.map((file) => `
      <li><span>${icon("file-up")}<span><strong title="${escapeHtml(file.name)}">${escapeHtml(file.name)}</strong><small>${formatBytes(file.size)}</small></span></span><button type="button" data-controller-file-queue-remove="${file.id}" aria-label="${fileQueueActions.matches("remove", file.id) ? "正在从队列移除" : "从队列移除"} ${escapeHtml(file.name)}" title="${fileQueueActions.matches("remove", file.id) ? "正在移除" : "从队列移除"}" ${queueActionsDisabled ? "disabled" : ""}>${fileQueueActions.matches("remove", file.id) ? icon("loader-circle", "button-spinner") : icon("x")}</button></li>
    `).join("")}</ul>
    ${fileQueue.queued.length > visible.length ? `<small class="remote-transfer-more">还有 ${fileQueue.queued.length - visible.length} 个文件正在等待</small>` : ""}
  `;
}

function renderFileQueueProtection(): string {
  const retrying = fileQueueActions.matches("protect");
  const protection = fileQueueProtectionPresentation(
    fileQueue.recoveryState,
    fileQueue.recoveryMessage,
    retrying,
  );
  if (!protection) return "";
  if (protection.tone === "protected") {
    return `<div class="remote-file-queue-protection" data-state="protected">${icon("shield-check")}<span>${protection.message}</span></div>`;
  }
  return `<div class="remote-file-queue-protection" data-state="memory-only" title="${escapeHtml(protection.message)}">${icon("shield-alert")}<span>${escapeHtml(protection.message)}</span><button type="button" data-controller-file-queue-protection-retry ${fileQueueActions.busy || protection.retryDisabled ? "disabled" : ""}>${icon(retrying ? "loader-circle" : "rotate-ccw", retrying ? "button-spinner" : "")}${protection.retryLabel}</button></div>`;
}

function renderTransferHistory(): string {
  const visible = transferHistory.slice(0, 4);
  return `
    <div class="remote-transfer-list-heading"><strong>最近传输</strong><button type="button" data-controller-history-clear>清除记录</button></div>
    <ul>${visible.map((entry) => `
      <li data-state="${entry.state}"><span>${icon(entry.direction === "download" ? "file-down" : "file-up")}<span><strong title="${escapeHtml(entry.name)}">${escapeHtml(entry.name)}</strong><small>${transferHistoryLabel(entry)} · ${formatHistoryTime(entry.finishedAtMs)}</small></span></span></li>
    `).join("")}</ul>
  `;
}

function transferHistoryLabel(entry: TransferHistoryEntry): string {
  const action = entry.direction === "download" ? "接收" : "发送";
  switch (entry.state) {
    case "completed": return `${action}完成${entry.size > 0 ? ` · ${formatBytes(entry.size)}` : ""}`;
    case "rejected": return `${action}被拒绝`;
    case "cancelled": return `${action}已取消`;
    default: return `${action}失败`;
  }
}

function formatHistoryTime(value: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  }).format(new Date(value));
}

function updateDeviceSubmitState(): void {
  const submit = document.querySelector<HTMLButtonElement>("[data-controller-device-submit]");
  if (submit) {
    submit.disabled = busy || cancelling || !deviceCredentialsAreValid(deviceIdDraft, temporaryPasswordDraft);
  }
}

async function submitDevice(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  if (busy || cancelling) {
    return;
  }
  const form = event.currentTarget as HTMLFormElement;
  const data = new FormData(form);
  deviceIdDraft = formatDeviceId(String(data.get("deviceId") ?? ""));
  temporaryPasswordDraft = normalizeTemporaryPassword(String(data.get("temporaryPassword") ?? ""));
  if (!deviceCredentialsAreValid(deviceIdDraft, temporaryPasswordDraft)) {
    feedback = { tone: "error", message: "请输入完整的 12 位设备 ID 和 8 位访问密码。" };
    requestRender();
    return;
  }
  await beginConnection(
    (channels) =>
      connectDevice(
        { deviceId: deviceIdDraft, temporaryPassword: temporaryPasswordDraft },
        channels,
      ),
    deviceIdDraft,
  );
}

async function beginSavedConnection(): Promise<void> {
  await beginConnection(
    (channels) => reconnectController(channels),
    snapshot?.savedConnection?.deviceId ?? "已批准设备",
  );
}

async function beginStoredDeviceConnection(deviceId: string): Promise<void> {
  deviceIdDraft = formatDeviceId(deviceId);
  await beginConnection(
    (channels) => connectSavedDevice({ deviceId: deviceIdDraft }, channels),
    deviceIdDraft,
  );
}

async function beginConnection(
  operation: (channels: ControllerChannels) => Promise<ControllerSnapshot>,
  target: string,
): Promise<boolean> {
  if (busy || cancelling) {
    return false;
  }
  busy = true;
  feedback = null;
  forgetConfirmation = null;
  clearRenameDraft();
  attemptStartedAtMs = Date.now();
  attemptTarget = target;
  videoConfig = null;
  failedVideoConfig = null;
  remoteAudio.release();
  audioStatus = "starting";
  audioEnabled = true;
  audioToggleBusy = false;
  audioMessage = "正在准备远端系统声音。";
  remoteAudio.setEnabled(true);
  remoteAudio.prepare();
  resetVideoTelemetry();
  prepareControllerRender();
  const generation = ++channelGeneration;
  const channels = createControllerChannels(
    (signal) => {
      if (generation === channelGeneration) {
        handleSignal(signal);
      }
    },
    (payload) => {
      if (generation === channelGeneration) {
        remoteAudio.push(payload);
      }
    },
    () => {
      if (generation === channelGeneration) {
        remoteAudio.resetConnection();
      }
    },
  );
  activeChannels = channels;
  requestRender();
  let started = false;
  try {
    const nextSnapshot = await operation(channels);
    if (generation !== channelGeneration) {
      return false;
    }
    snapshot = nextSnapshot;
    if (fileRecoveryAvailable && isFileTransferRetryable(fileTransfer?.state)) {
      const recoveryTarget = nextSnapshot.fileRecovery?.deviceId;
      if (!recoveryTarget || deviceIdsMatch(recoveryTarget, target)) {
        transferPanelOpen = true;
      } else {
        feedback = {
          tone: "info",
          message: `上次文件任务属于设备 ${recoveryTarget}，当前连接不会恢复或发送该任务。`,
        };
      }
    }
    const queueRecoveryTarget = nextSnapshot.fileQueueRecovery?.deviceId;
    if (queueRecoveryTarget) {
      transferPanelOpen = true;
      if (!deviceIdsMatch(queueRecoveryTarget, target)) {
        feedback = {
          tone: "info",
          message: `设备 ${queueRecoveryTarget} 仍有等待发送文件；它们不会发送到当前电脑，可在传输面板中放弃旧队列。`,
        };
      }
    }
    started = true;
  } catch (error) {
    if (generation !== channelGeneration) {
      return false;
    }
    feedback = { tone: "error", message: normalizeError(error) };
    try {
      snapshot = await getControllerSnapshot();
    } catch {
      // Keep the last known state; the actionable connection error remains visible.
    }
  } finally {
    if (generation === channelGeneration) {
      busy = false;
      if (snapshot && !isActiveConnectionState(snapshot.runtime.state) && snapshot.runtime.state !== "connected") {
        clearConnectionAttempt();
      }
      requestRender();
    }
  }
  return started;
}

async function cancelConnection(): Promise<void> {
  if (cancelling) {
    return;
  }
  const wasConnected = snapshot?.runtime.state === "connected";
  if (remoteFullscreenActive || remoteFullscreenDesired) {
    void setRemoteFullscreen(false, false);
  }
  const generation = ++channelGeneration;
  busy = false;
  cancelling = true;
  prepareControllerRender();
  remoteAudio.release();
  audioStatus = "starting";
  audioMessage = "正在准备远端系统声音。";
  videoConfig = null;
  resetVideoTelemetry();
  activeChannels = null;
  feedback = { tone: "info", message: wasConnected ? "正在断开远程控制…" : "正在取消本次连接…" };
  requestRender();
  try {
    const nextSnapshot = await disconnectController();
    if (generation !== channelGeneration) {
      return;
    }
    snapshot = nextSnapshot;
    feedback = {
      tone: "success",
      message: wasConnected
        ? "远程控制已结束，已批准的电脑仍可重新连接。"
        : "已取消连接。设备 ID 和访问密码仍保留，可以直接重新尝试。",
    };
  } catch (error) {
    if (generation === channelGeneration) {
      feedback = { tone: "error", message: normalizeError(error) };
    }
  } finally {
    if (generation === channelGeneration) {
      cancelling = false;
      clearConnectionAttempt();
      requestRender();
    }
  }
}

async function removeStoredDevice(deviceId: string): Promise<void> {
  if (busy || cancelling) {
    return;
  }
  busy = true;
  forgetConfirmation = null;
  clearRenameDraft();
  feedback = null;
  requestRender();
  try {
    snapshot = await forgetSavedDevice({ deviceId });
    feedback = { tone: "success", message: `已移除设备 ${deviceId} 的加密密码。` };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    busy = false;
    requestRender();
  }
}

async function submitDeviceRename(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  if (!renameDeviceId || renameBusy || busy || cancelling) {
    return;
  }
  const form = event.currentTarget as HTMLFormElement;
  const data = new FormData(form);
  renameDraft = String(data.get("alias") ?? "").trim();
  const deviceId = renameDeviceId;
  renameBusy = true;
  feedback = null;
  requestRender();
  try {
    snapshot = await renameSavedDevice({ deviceId, alias: renameDraft });
    feedback = {
      tone: "success",
      message: renameDraft
        ? `设备 ${deviceId} 已命名为“${renameDraft}”。`
        : `设备 ${deviceId} 已恢复显示设备 ID。`,
    };
    clearRenameDraft();
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    renameBusy = false;
    requestRender();
  }
}

function clearRenameDraft(): void {
  renameDeviceId = null;
  renameDraft = "";
}

async function clearStoredDevices(): Promise<void> {
  if (busy || cancelling) {
    return;
  }
  busy = true;
  forgetConfirmation = null;
  clearRenameDraft();
  feedback = null;
  requestRender();
  try {
    snapshot = await clearSavedDevices();
    feedback = { tone: "success", message: "已清除异常的加密设备记录，现在可以重新输入设备密码。" };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    busy = false;
    requestRender();
  }
}

function handleSignal(signal: ControllerSignal): void {
  switch (signal.kind) {
    case "status": {
      const previousRuntime = snapshot?.runtime;
      const presentationChanged = !previousRuntime
        || previousRuntime.state !== signal.runtime.state
        || previousRuntime.title !== signal.runtime.title
        || previousRuntime.detail !== signal.runtime.detail
        || previousRuntime.streamId !== signal.runtime.streamId;
      if (snapshot) {
        snapshot.runtime = signal.runtime;
      } else {
        snapshot = {
          runtime: signal.runtime,
          savedConnection: null,
          connectionError: null,
          savedDevices: [],
          savedDevicesError: null,
          fileRecovery: null,
          fileRecoveryError: null,
          fileQueueRecovery: null,
          fileQueueRecoveryError: null,
        };
      }
      if (signal.runtime.state !== "connected") {
        if (remoteFullscreenActive || remoteFullscreenDesired) {
          void setRemoteFullscreen(false, false);
        }
        resetRemoteToolbarState();
        smartPasteBusy = false;
        smartPastePending = false;
        releaseInputState();
        inputDispatcher.discardAll();
        if (pointerFrame !== null) {
          window.cancelAnimationFrame(pointerFrame);
          pointerFrame = null;
        }
        pointerInsideViewport = false;
        remotePanMode = false;
        remoteCanvasBounds = null;
        remoteViewportBounds = null;
        setFileDragActive(false);
        videoConfig = null;
        failedVideoConfig = null;
        resetVideoTelemetry();
        remoteDisplays = [];
        activeRemoteDisplayId = null;
        clearRemoteDisplaySwitch();
        clearVideoQualityAckTimer();
        videoQualityPreference = "automatic";
        appliedVideoQuality = "sharp";
        pendingVideoQuality = null;
        remoteAudio.resetConnection();
        audioStatus = "starting";
        audioMessage = "正在等待远端系统声音。";
        fileTransferMetrics = null;
        lastTransferProgressPaintAtMs = null;
        cancelScheduledTransferPanelUpdate();
      }
      if (signal.runtime.state === "connected") {
        temporaryPasswordDraft = "";
      }
      if (["idle", "connected", "stopped"].includes(signal.runtime.state)) {
        clearConnectionAttempt();
      }
      if (presentationChanged) {
        requestRender();
      }
      break;
    }
    case "videoConfig": {
      const changed = !videoConfig || videoConfigKey(videoConfig) !== videoConfigKey(signal);
      if (changed) {
        resetVideoTelemetry();
        videoConfigReceivedAtMs = Date.now();
      }
      videoConfig = signal;
      if (changed || !document.querySelector("[data-remote-canvas]")) {
        requestRender();
      }
      break;
    }
    case "cursor":
      updateRemoteCursor(signal.xMillionths, signal.yMillionths, signal.visible);
      break;
    case "displays": {
      const outcome = remoteDisplaySwitch.acknowledge(signal.activeDisplayId);
      clearRemoteDisplaySwitchTimer();
      remoteDisplays = signal.displays;
      activeRemoteDisplayId = signal.activeDisplayId;
      if (outcome === "applied") {
        const index = remoteDisplays.findIndex((display) => display.id === activeRemoteDisplayId);
        remoteDisplaySwitchStatus = {
          tone: "success",
          message: index >= 0 ? `已切换到屏幕 ${index + 1}` : "已切换远程屏幕",
        };
      } else if (outcome === "rejected") {
        remoteDisplaySwitchStatus = {
          tone: "error",
          message: "目标屏幕暂时不可用，仍保持当前屏幕。",
        };
      }
      updateRemoteDisplayControls();
      break;
    }
    case "videoQuality": {
      clearVideoQualityAckTimer();
      videoQualityPreference = signal.preference;
      appliedVideoQuality = signal.preset;
      pendingVideoQuality = null;
      const picker = document.querySelector<HTMLSelectElement>("[data-controller-video-quality]");
      if (picker) {
        const automatic = picker.querySelector<HTMLOptionElement>('option[value="automatic"]');
        if (automatic) {
          automatic.textContent = `自动（${videoQualityPresetLabel(appliedVideoQuality)}）`;
        }
        picker.value = videoQualityPreference;
        picker.disabled = false;
      }
      updateRemoteSessionSummary();
      break;
    }
    case "metrics": {
      relayCompletedFrames = signal.completedFrames;
      updateVideoStartingMessage();
      reportRenderMetrics();
      break;
    }
    case "clipboard":
      clipboardTransfer = signal;
      if (
        smartPastePending
        && signal.operation === "paste"
        && (signal.state === "completed" || signal.state === "failed")
      ) {
        smartPasteBusy = false;
        smartPastePending = false;
      }
      updateTransferPanel({ ledger: false });
      break;
    case "fileTransfer": {
      const previous = fileTransfer;
      fileTransfer = signal;
      fileRecoveryAvailable = fileRecoveryAvailabilityAfterSignal(
        fileRecoveryAvailable,
        signal.state,
      );
      updateFileTransferMetrics(signal);
      const historyChanged = recordTransferHistory(previous, signal);
      transferActivity = recordTransferResult(transferActivity, previous, signal, transferPanelOpen);
      scheduleTransferPanelUpdate(previous, signal, historyChanged);
      break;
    }
    case "fileQueue": {
      const previouslyQueued = fileQueue.queued.length;
      fileQueue = {
        queued: signal.queued,
        paused: signal.paused,
        recoveryState: signal.recoveryState,
        recoveryMessage: signal.recoveryMessage,
      };
      if (
        previouslyQueued > 0
        && signal.queued.length === 0
        && signal.recoveryState === "empty"
        && snapshot?.fileQueueRecovery
        && deviceIdsMatch(snapshot.fileQueueRecovery.deviceId, attemptTarget || deviceIdDraft)
      ) {
        snapshot = {
          ...snapshot,
          fileQueueRecovery: null,
          fileQueueRecoveryError: null,
        };
      }
      updateTransferPanel({ ledger: true });
      break;
    }
    case "audio":
      audioStatus = signal.state;
      audioEnabled = signal.enabled;
      audioMessage = signal.message;
      remoteAudio.setEnabled(signal.state === "enabled" && signal.enabled);
      updateAudioControl();
      break;
  }
}

async function toggleRemoteAudio(): Promise<void> {
  if (
    audioToggleBusy
    || audioStatus === "starting"
    || audioStatus === "unavailable"
  ) {
    return;
  }
  const previousEnabled = audioEnabled;
  const previousStatus = audioStatus;
  const previousMessage = audioMessage;
  const nextEnabled = !audioEnabled;
  audioToggleBusy = true;
  audioEnabled = nextEnabled;
  audioStatus = nextEnabled ? "enabled" : "muted";
  audioMessage = nextEnabled ? "正在播放远端系统声音。" : "远端系统声音已静音。";
  remoteAudio.setEnabled(nextEnabled);
  updateAudioControl();
  try {
    await setControllerAudioEnabled(nextEnabled);
  } catch (error) {
    audioEnabled = previousEnabled;
    audioStatus = previousStatus;
    audioMessage = previousMessage;
    remoteAudio.setEnabled(previousStatus === "enabled" && previousEnabled);
    feedback = { tone: "error", message: normalizeError(error) };
    requestRender();
  } finally {
    audioToggleBusy = false;
    updateAudioControl();
  }
}

function updateAudioControl(): void {
  const button = document.querySelector<HTMLButtonElement>("[data-controller-audio]");
  if (!button) return;
  const unavailable = audioStatus === "starting" || audioStatus === "unavailable";
  const muted = audioStatus === "muted" || !audioEnabled;
  const iconName = unavailable ? "volume-off" : muted ? "volume-x" : "volume-2";
  const label = audioStatus === "starting"
    ? "声音准备中"
    : audioStatus === "unavailable"
      ? "无声音"
      : muted
        ? "打开声音"
        : "静音";
  button.disabled = unavailable || audioToggleBusy;
  button.title = audioMessage;
  button.setAttribute("aria-pressed", String(muted));
  button.innerHTML = icon(iconName) + '<span data-controller-audio-label>' + label + "</span>";
  renderLucideIcons(button);
}

function updateTransferPanel({ ledger = true }: { ledger?: boolean } = {}): void {
  const panel = document.querySelector<HTMLElement>("[data-controller-transfer-panel]");
  const toggle = document.querySelector<HTMLButtonElement>("[data-controller-transfer]");
  if (panel) {
    panel.hidden = !transferPanelOpen;
  }
  toggle?.setAttribute("aria-expanded", String(transferPanelOpen));
  updateTransferToolbarActivity();
  if (!transferPanelOpen) {
    return;
  }

  const clipboardStatus = document.querySelector<HTMLElement>("[data-controller-clipboard-status]");
  if (clipboardStatus) {
    clipboardStatus.hidden = !clipboardTransfer;
    clipboardStatus.dataset.state = clipboardTransfer?.state ?? "";
  }
  const clipboardMessage = document.querySelector<HTMLElement>("[data-controller-clipboard-message]");
  if (clipboardMessage) {
    clipboardMessage.textContent = clipboardTransfer?.message ?? "";
  }

  const fileStatus = document.querySelector<HTMLElement>("[data-controller-file-status]");
  if (fileStatus) {
    fileStatus.hidden = !fileTransfer;
    fileStatus.dataset.state = fileTransfer?.state ?? "";
  }
  const fileName = document.querySelector<HTMLElement>("[data-controller-file-name]");
  const fileMessage = document.querySelector<HTMLElement>("[data-controller-file-message]");
  const fileSize = document.querySelector<HTMLElement>("[data-controller-file-size]");
  if (fileName) fileName.textContent = fileTransfer?.name ?? "";
  if (fileMessage) fileMessage.textContent = fileTransfer?.message ?? "";
  if (fileSize) {
    fileSize.textContent = fileTransfer
      ? formatTransferDetails(fileTransfer)
      : "";
  }
  const progress = document.querySelector<HTMLProgressElement>("[data-controller-file-progress]");
  if (progress) progress.value = transferPercent(fileTransfer?.transferred ?? 0, fileTransfer?.total ?? 0);
  const cancel = document.querySelector<HTMLButtonElement>("[data-controller-file-cancel]");
  if (cancel) cancel.hidden = !isFileTransferActive(fileTransfer?.state);
  const retry = document.querySelector<HTMLButtonElement>("[data-controller-file-retry]");
  if (retry) {
    retry.hidden = !(fileRecoveryAvailable && isFileTransferRetryable(fileTransfer?.state));
    retry.disabled = discardFileRecoveryBusy || discardFileQueueRecoveryBusy || fileQueueActions.busy;
    retry.innerHTML = `${icon("rotate-ccw")}${fileTransfer?.direction === "download" ? "重新获取" : "重新发送"}`;
    renderLucideIcons(retry);
  }
  const discard = document.querySelector<HTMLButtonElement>("[data-controller-file-discard]");
  if (discard) {
    discard.hidden = !(fileRecoveryAvailable && isFileTransferRetryable(fileTransfer?.state));
    discard.disabled = discardFileRecoveryBusy;
    discard.innerHTML = `${icon("trash-2")}${discardFileRecoveryBusy ? "正在清理…" : "不再重试"}`;
    renderLucideIcons(discard);
  }
  const sendFile = document.querySelector<HTMLButtonElement>("[data-controller-file-send]");
  if (sendFile) {
    sendFile.disabled = filePickerBusy || discardFileRecoveryBusy || discardFileQueueRecoveryBusy || fileQueueActions.busy;
    sendFile.innerHTML = `${icon("file-up")}${filePickerBusy ? "正在选择…" : "添加发送文件"}`;
    renderLucideIcons(sendFile);
  }
  const receiveFile = document.querySelector<HTMLButtonElement>("[data-controller-file-receive]");
  if (receiveFile) {
    receiveFile.disabled = isFileTransferActive(fileTransfer?.state)
      || fileQueue.queued.length > 0
      || discardFileRecoveryBusy
      || discardFileQueueRecoveryBusy
      || fileQueueActions.busy;
  }
  if (ledger) {
    const recovery = document.querySelector<HTMLElement>("[data-controller-file-queue-recovery]");
    if (recovery) {
      recovery.outerHTML = renderFileQueueRecovery();
      const nextRecovery = document.querySelector<HTMLElement>("[data-controller-file-queue-recovery]");
      if (nextRecovery) renderLucideIcons(nextRecovery);
    }
    const queue = document.querySelector<HTMLElement>("[data-controller-file-queue]");
    if (queue) {
      queue.hidden = fileQueue.queued.length === 0;
      queue.innerHTML = renderFileQueue();
      renderLucideIcons(queue);
    }
    const history = document.querySelector<HTMLElement>("[data-controller-file-history]");
    if (history) {
      history.hidden = transferHistory.length === 0;
      history.innerHTML = renderTransferHistory();
      renderLucideIcons(history);
    }
    bindTransferLedgerActions();
  }
  updateDownloadsFolderAction();
}

function openTransferPanel(): void {
  transferPanelOpen = true;
  transferActivity = markTransferResultsRead(transferActivity);
  revealRemoteToolbar();
}

function recordTransferHistory(
  previous: FileTransferStatus | null,
  next: FileTransferStatus,
): boolean {
  const nextSequence = transferHistorySequence + 1;
  const updated = appendTransferHistory(
    transferHistory,
    previous,
    next,
    nextSequence,
    Date.now(),
  );
  if (updated !== transferHistory) {
    transferHistorySequence = nextSequence;
    transferHistory = updated;
    return true;
  }
  return false;
}

function scheduleTransferPanelUpdate(
  previous: FileTransferStatus | null,
  next: FileTransferStatus,
  ledger: boolean,
): void {
  updateTransferToolbarActivity();
  if (!transferPanelOpen) {
    cancelScheduledTransferPanelUpdate();
    return;
  }
  const nowMs = performance.now();
  const delay = transferProgressPaintDelay(
    previous?.state ?? null,
    next.state,
    lastTransferProgressPaintAtMs,
    nowMs,
  );
  if (delay === 0) {
    cancelScheduledTransferPanelUpdate();
    lastTransferProgressPaintAtMs = nowMs;
    updateTransferPanel({ ledger });
    return;
  }
  if (transferPanelUpdateTimer !== null) {
    return;
  }
  transferPanelUpdateTimer = window.setTimeout(() => {
    transferPanelUpdateTimer = null;
    if (!transferPanelOpen) return;
    lastTransferProgressPaintAtMs = performance.now();
    updateTransferPanel({ ledger: false });
  }, delay);
}

function cancelScheduledTransferPanelUpdate(): void {
  if (transferPanelUpdateTimer === null) return;
  window.clearTimeout(transferPanelUpdateTimer);
  transferPanelUpdateTimer = null;
}

function updateTransferToolbarActivity(): void {
  const button = document.querySelector<HTMLButtonElement>("[data-controller-transfer]");
  const badge = document.querySelector<HTMLElement>("[data-controller-transfer-activity]");
  if (!button || !badge) return;
  const activity = transferToolbarActivityView();
  button.classList.toggle("toolbar-button--has-activity", activity.visible);
  button.title = activity.title;
  badge.hidden = !activity.visible;
  badge.dataset.state = activity.tone;
  badge.textContent = activity.badge;
  badge.setAttribute("aria-label", activity.ariaLabel);
}

function updateFileTransferMetrics(status: FileTransferStatus): void {
  if ((status.state !== "sending" && status.state !== "receiving") || status.total <= 0) {
    fileTransferMetrics = null;
    return;
  }
  fileTransferMetrics = sampleTransferMetrics(fileTransferMetrics, {
    identity: transferIdentity(status),
    state: status.state,
    transferred: status.transferred,
    total: status.total,
  }, performance.now());
}

function transferIdentity(status: FileTransferStatus): string {
  return `${status.direction}\u0000${status.name}\u0000${status.total}`;
}

function hasCompletedDownload(): boolean {
  return (fileTransfer?.direction === "download" && fileTransfer.state === "completed")
    || transferHistory.some((entry) => entry.direction === "download" && entry.state === "completed");
}

function updateDownloadsFolderAction(): void {
  const action = document.querySelector<HTMLElement>(".remote-downloads-action");
  const button = document.querySelector<HTMLButtonElement>("[data-controller-downloads-open]");
  const message = document.querySelector<HTMLElement>("[data-controller-downloads-message]");
  if (action) action.hidden = !hasCompletedDownload();
  if (button) {
    button.disabled = downloadsFolderBusy;
    button.innerHTML = `${icon("folder-open")}${downloadsFolderBusy ? "正在打开…" : "打开下载文件夹"}`;
    renderLucideIcons(button);
  }
  if (message) message.textContent = downloadsFolderMessage;
}

async function openDownloadsFolder(): Promise<void> {
  if (downloadsFolderBusy) return;
  downloadsFolderBusy = true;
  downloadsFolderMessage = "";
  updateDownloadsFolderAction();
  try {
    await openControllerDownloadsFolder();
    downloadsFolderMessage = "已打开";
  } catch (error) {
    downloadsFolderMessage = normalizeError(error);
  } finally {
    downloadsFolderBusy = false;
    updateDownloadsFolderAction();
  }
}

function bindTransferLedgerActions(): void {
  document.querySelector<HTMLButtonElement>("[data-controller-file-queue-recovery-discard]")
    ?.addEventListener("click", () => void discardFileQueueRecovery());
  document.querySelector<HTMLButtonElement>("[data-controller-file-queue-clear]")
    ?.addEventListener("click", () => void clearQueuedFiles());
  document.querySelector<HTMLButtonElement>("[data-controller-file-queue-resume]")
    ?.addEventListener("click", () => void resumeQueuedFiles());
  document.querySelector<HTMLButtonElement>("[data-controller-file-queue-protection-retry]")
    ?.addEventListener("click", () => void retryQueuedFilesProtection());
  document.querySelectorAll<HTMLButtonElement>("[data-controller-file-queue-remove]").forEach((button) => {
    button.addEventListener("click", () => {
      const transferId = button.dataset.controllerFileQueueRemove;
      if (transferId) void removeQueuedFile(transferId);
    });
  });
  document.querySelector<HTMLButtonElement>("[data-controller-history-clear]")
    ?.addEventListener("click", () => {
      transferHistory = [];
      transferActivity = markTransferResultsRead(transferActivity);
      updateTransferPanel();
    });
}

async function sendLocalClipboard(): Promise<void> {
  if (isClipboardTransferActive(clipboardTransfer?.state)) return;
  openTransferPanel();
  clipboardTransfer = {
    kind: "clipboard",
    state: "sending",
    operation: "send",
    message: "正在读取并发送本机剪贴板…",
  };
  updateTransferPanel();
  try {
    await sendControllerClipboard();
  } catch (error) {
    clipboardTransfer = {
      kind: "clipboard",
      state: "failed",
      operation: "send",
      message: normalizeError(error),
    };
    updateTransferPanel();
  }
}

async function receiveRemoteClipboard(): Promise<void> {
  if (isClipboardTransferActive(clipboardTransfer?.state)) return;
  openTransferPanel();
  clipboardTransfer = {
    kind: "clipboard",
    state: "receiving",
    operation: "receive",
    message: "正在读取远端剪贴板…",
  };
  updateTransferPanel();
  try {
    await requestControllerClipboard();
  } catch (error) {
    clipboardTransfer = {
      kind: "clipboard",
      state: "failed",
      operation: "receive",
      message: normalizeError(error),
    };
    updateTransferPanel();
  }
}

async function chooseFileForTransfer(): Promise<void> {
  if (filePickerBusy || discardFileRecoveryBusy || discardFileQueueRecoveryBusy || fileQueueActions.busy) return;
  openTransferPanel();
  filePickerBusy = true;
  updateTransferPanel();
  try {
    await chooseAndSendControllerFile();
  } catch (error) {
    fileTransfer = {
      kind: "fileTransfer",
      state: "failed",
      direction: "upload",
      name: "文件传输",
      transferred: 0,
      total: 0,
      message: normalizeError(error),
    };
  } finally {
    filePickerBusy = false;
    updateTransferPanel();
  }
}

async function enqueueDroppedFiles(paths: string[]): Promise<void> {
  if (paths.length === 0 || filePickerBusy || discardFileRecoveryBusy || discardFileQueueRecoveryBusy || fileQueueActions.busy) return;
  openTransferPanel();
  updateTransferPanel();
  try {
    await queueControllerFiles(paths);
  } catch (error) {
    showFileQueueError(error);
  }
}

async function removeQueuedFile(transferId: string): Promise<void> {
  await runFileQueueAction("remove", () => removeControllerQueuedFile(transferId), transferId);
}

async function clearQueuedFiles(): Promise<void> {
  await runFileQueueAction("clear", clearControllerFileQueue);
}

async function resumeQueuedFiles(): Promise<void> {
  await runFileQueueAction("resume", resumeControllerFileQueue);
}

async function retryQueuedFilesProtection(): Promise<void> {
  if (fileQueue.recoveryState !== "memoryOnly") return;
  await runFileQueueAction("protect", retryControllerFileQueueProtection);
}

async function runFileQueueAction(
  kind: FileQueueActionKind,
  operation: () => Promise<void>,
  transferId: string | null = null,
): Promise<void> {
  if (filePickerBusy || discardFileRecoveryBusy || discardFileQueueRecoveryBusy) return;
  const action = fileQueueActions.begin(kind, transferId);
  if (!action) return;
  updateTransferPanel();
  try {
    await operation();
  } catch (error) {
    showFileQueueError(error);
  } finally {
    finishFileQueueAction(action);
  }
}

function finishFileQueueAction(action: FileQueueActionToken): void {
  if (!fileQueueActions.finish(action)) return;
  updateTransferPanel();
}

async function discardFileQueueRecovery(): Promise<void> {
  if (filePickerBusy || discardFileQueueRecoveryBusy || fileQueueActions.busy) return;
  discardFileQueueRecoveryBusy = true;
  updateTransferPanel();
  try {
    const latestSnapshot = await getControllerSnapshot();
    const recovery = latestSnapshot.fileQueueRecovery;
    if (recovery) {
      await discardControllerFileQueueRecovery(recovery.revision);
    }
    snapshot = {
      ...latestSnapshot,
      fileQueueRecovery: null,
      fileQueueRecoveryError: null,
    };
    feedback = { tone: "success", message: "旧的等待发送队列已清理，未删除任何本机文件。" };
  } catch (error) {
    showFileQueueError(error);
  } finally {
    discardFileQueueRecoveryBusy = false;
    updateTransferPanel();
  }
}

function showFileQueueError(error: unknown): void {
  if (!isFileTransferActive(fileTransfer?.state)) {
    fileTransfer = {
      kind: "fileTransfer",
      state: "failed",
      direction: "upload",
      name: "文件队列",
      transferred: 0,
      total: 0,
      message: normalizeError(error),
    };
  }
  updateTransferPanel();
}

async function initializeNativeFileDrop(): Promise<void> {
  if (fileDropInitialized) return;
  fileDropInitialized = true;
  try {
    await getCurrentWebview().onDragDropEvent((event) => {
      const connected = snapshot?.runtime.state === "connected";
      if (!connected) {
        setFileDragActive(false);
        return;
      }
      if (event.payload.type === "enter" || event.payload.type === "over") {
        setFileDragActive(true);
      } else if (event.payload.type === "leave") {
        setFileDragActive(false);
      } else if (event.payload.type === "drop") {
        setFileDragActive(false);
        void enqueueDroppedFiles(event.payload.paths);
      }
    });
  } catch {
    fileDropInitialized = false;
  }
}

function setFileDragActive(active: boolean): void {
  fileDragActive = active;
  const overlay = document.querySelector<HTMLElement>("[data-controller-file-drop-overlay]");
  if (overlay) {
    overlay.hidden = !active;
  }
}

async function requestRemoteFileTransfer(): Promise<void> {
  if (
    isFileTransferActive(fileTransfer?.state)
    || fileQueue.queued.length > 0
    || discardFileRecoveryBusy
  ) return;
  openTransferPanel();
  fileTransfer = {
    kind: "fileTransfer",
    state: "waiting",
    direction: "download",
    name: "等待远端选择文件",
    transferred: 0,
    total: 0,
    message: "正在请求远端电脑选择文件…",
  };
  updateTransferPanel();
  try {
    await requestControllerRemoteFile();
  } catch (error) {
    fileTransfer = { ...fileTransfer, state: "failed", message: normalizeError(error) };
    updateTransferPanel();
  }
}

async function cancelFileTransfer(): Promise<void> {
  if (!isFileTransferActive(fileTransfer?.state)) return;
  try {
    await cancelControllerFile();
  } catch (error) {
    fileTransfer = fileTransfer
      ? { ...fileTransfer, state: "failed", message: normalizeError(error) }
      : null;
    updateTransferPanel();
  }
}

async function retryFileTransfer(): Promise<void> {
  if (!fileTransfer || !fileRecoveryAvailable || !isFileTransferRetryable(fileTransfer.state)) return;
  const previous = fileTransfer;
  fileTransfer = {
    ...previous,
    state: "waiting",
    transferred: 0,
    message: previous.direction === "download"
      ? "正在重新请求远端选择文件…"
      : "正在重新发送，等待远端确认…",
  };
  updateTransferPanel();
  try {
    await retryControllerFile();
  } catch (error) {
    fileTransfer = { ...previous, state: "failed", message: normalizeError(error) };
    updateTransferPanel();
  }
}

async function discardFileRecovery(): Promise<void> {
  if (
    !fileTransfer
    || !fileRecoveryAvailable
    || !isFileTransferRetryable(fileTransfer.state)
    || discardFileRecoveryBusy
  ) {
    return;
  }
  const previous = fileTransfer;
  discardFileRecoveryBusy = true;
  updateTransferPanel();
  try {
    const latestSnapshot = await getControllerSnapshot();
    const recovery = latestSnapshot.fileRecovery;
    if (recovery) {
      await discardControllerFileRecovery(recovery.revision);
    }
    fileTransfer = null;
    fileRecoveryAvailable = false;
    fileTransferMetrics = null;
    snapshot = {
      ...latestSnapshot,
      fileRecovery: null,
      fileRecoveryError: null,
    };
  } catch (error) {
    fileTransfer = { ...previous, state: "failed", message: normalizeError(error) };
  } finally {
    discardFileRecoveryBusy = false;
    updateTransferPanel();
  }
}

function isFileTransferActive(state: FileTransferStatus["state"] | undefined): boolean {
  return state === "waiting" || state === "sending" || state === "receiving" || state === "verifying";
}

function isClipboardTransferActive(state: ClipboardTransferStatus["state"] | undefined): boolean {
  return state === "sending" || state === "receiving";
}

function isFileTransferRetryable(state: FileTransferStatus["state"] | undefined): boolean {
  return state === "failed" || state === "cancelled";
}

function formatTransferBytes(transferred: number, total: number): string {
  if (total <= 0) return "";
  return `${formatBytes(Math.min(transferred, total))} / ${formatBytes(total)}`;
}

function formatTransferDetails(status: FileTransferStatus): string {
  const size = formatTransferBytes(status.transferred, status.total);
  const metrics = status.state === "sending" || status.state === "receiving"
    ? transferMetricsLabel(fileTransferMetrics, status.transferred, status.total)
    : "";
  return [size, metrics].filter(Boolean).join(" · ");
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function transferPercent(transferred: number, total: number): number {
  if (total <= 0) return transferred > 0 ? 100 : 0;
  return Math.min(100, Math.max(0, Math.round((transferred / total) * 100)));
}

function isActiveConnectionState(state: string): boolean {
  return ["finding", "connecting", "waitingApproval", "reconnecting"].includes(state);
}

function connectionActionLabel(state: string): string {
  switch (state) {
    case "waitingApproval":
      return "等待主机确认";
    case "reconnecting":
      return "正在重试连接";
    case "connecting":
      return "正在建立安全连接";
    default:
      return "正在查找设备";
  }
}

function connectionElapsedSeconds(): number {
  return attemptStartedAtMs === null ? 0 : Math.max(0, Math.floor((Date.now() - attemptStartedAtMs) / 1000));
}

function clearConnectionAttempt(): void {
  attemptStartedAtMs = null;
  attemptTarget = "";
}

function updateConnectionProgressClock(): void {
  const elapsed = document.querySelector<HTMLElement>("[data-controller-attempt-elapsed]");
  const guidance = document.querySelector<HTMLElement>("[data-controller-attempt-guidance]");
  const runtimeState = snapshot?.runtime.state;
  if (!elapsed || !guidance || !runtimeState || attemptStartedAtMs === null) {
    return;
  }
  const elapsedSeconds = connectionElapsedSeconds();
  const progressState = isActiveConnectionState(runtimeState) ? runtimeState : "finding";
  const presentation = connectionProgressPresentation(progressState, elapsedSeconds);
  elapsed.textContent = formatConnectionElapsed(elapsedSeconds);
  guidance.textContent = presentation.guidance;
  guidance.classList.toggle("connection-progress-guidance--delayed", presentation.delayed);
}

function setupRemoteDesktop(): void {
  const viewport = document.querySelector<HTMLElement>("[data-remote-viewport]");
  const canvas = document.querySelector<HTMLCanvasElement>("[data-remote-canvas]");
  if (!viewport || !canvas || !videoConfig) {
    return;
  }
  const config = videoConfig;
  const configKey = videoConfigKey(config);
  if (failedVideoConfig === configKey) {
    return;
  }
  if (typeof VideoDecoder === "undefined") {
    failedVideoConfig = configKey;
    feedback = { tone: "error", message: "当前 Windows WebView2 无法解码远程 H.264 画面。请更新 Microsoft Edge WebView2 Runtime 后重新打开 DeskLink。" };
    queueMicrotask(requestRender);
    return;
  }
  const context = canvas.getContext("2d", { alpha: false, desynchronized: true });
  if (!context) {
    failedVideoConfig = configKey;
    feedback = { tone: "error", message: "DeskLink 无法创建远程桌面绘制区域。" };
    queueMicrotask(requestRender);
    return;
  }
  setupRemoteGeometry(viewport, canvas);
  scheduleRemoteScaleLayout(viewport, canvas, remoteScaleMode === "actual");
  startVideoDecoder(canvas, context, configKey, "hardware");
  videoPull.start(
    { streamId: config.streamId, configVersion: config.configVersion },
    nextControllerVideoFrame,
    handleVideo,
    handleVideoDeliveryError,
    () => {
      videoPullFailures = nextVideoPullFailureCount(videoPullFailures);
    },
  );
  armVideoPlaybackPressureReporter(config.streamId);
  bindRemoteInput(viewport, canvas);
  viewport.focus({ preventScroll: true });
}

function handleVideo(payload: ControllerVideoPayload): void {
  if (!activeChannels || !videoConfig) {
    return;
  }
  const bytes = toUint8Array(payload);
  if (bytes.byteLength <= FRAME_PREFIX_BYTES) {
    throw new Error(`视频帧长度无效：${bytes.byteLength}`);
  }
  receivedVideoFrames += 1;
  const accessUnit = bytes.subarray(FRAME_PREFIX_BYTES);
  const keyframe = isH264Keyframe(accessUnit, bytes[0] === 1);
  if (!decoder || decoder.state !== "configured") {
    if (keyframe) {
      pendingVideoKeyframe = bytes.slice();
    }
    return;
  }
  submitVideoChunk(bytes);
}

function submitVideoChunk(bytes: Uint8Array): void {
  if (!decoder || decoder.state !== "configured" || !videoConfig) {
    return;
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const accessUnit = bytes.subarray(FRAME_PREFIX_BYTES);
  const keyframe = isH264Keyframe(accessUnit, bytes[0] === 1);
  if (awaitingDecoderKeyframe && !keyframe) {
    return;
  }
  if (videoPlaybackPressure.observe(decoder.decodeQueueSize, Date.now()) === "recover") {
    restartVideoDecoderForFreshness(videoConfigKey(videoConfig), decoderPreference);
    return;
  }
  const timestamp = Number(view.getBigUint64(1, true));
  const data = prepareH264AccessUnit(
    new Uint8Array(videoConfig.sequenceHeader),
    accessUnit,
    keyframe,
  );
  try {
    decoder.decode(new EncodedVideoChunk({
      type: keyframe ? "key" : "delta",
      timestamp,
      data,
    }));
    awaitingDecoderKeyframe = false;
    decoderSubmittedSinceStart += 1;
    submittedVideoFrames += 1;
    armDecoderStallWatch();
  } catch (error) {
    handleVideoDeliveryError(error);
    void requestControllerKeyframe().catch(showOperationError);
  }
}

function restartVideoDecoderForFreshness(
  configKey: string,
  preference: "hardware" | "software",
): void {
  if (!videoConfig || videoConfigKey(videoConfig) !== configKey || decoderPreference !== preference) {
    return;
  }
  const canvas = document.querySelector<HTMLCanvasElement>("[data-remote-canvas]");
  const context = canvas?.getContext("2d", { alpha: false, desynchronized: true });
  if (!canvas || !context) {
    return;
  }
  startVideoDecoder(canvas, context, configKey, preference);
}

function armVideoPlaybackPressureReporter(streamId: number): void {
  if (videoPlaybackPressureTimer !== null) {
    return;
  }
  videoPlaybackPressureTimer = window.setTimeout(() => {
    videoPlaybackPressureTimer = null;
    if (!videoConfig || videoConfig.streamId !== streamId) {
      return;
    }
    const sample = videoPlaybackPressure.takeSample();
    const report = sample.peakDecodeQueueSize > 0 || sample.freshnessRecoveries > 0
      ? reportControllerPlaybackPressure({ streamId, ...sample }).catch(() => {
          // Playback feedback is best-effort and must never interrupt the live session.
        })
      : Promise.resolve();
    void report.finally(() => {
      if (videoConfig?.streamId === streamId) {
        armVideoPlaybackPressureReporter(streamId);
      }
    });
  }, VIDEO_PRESSURE_REPORT_INTERVAL_MS);
}

function handleVideoDeliveryError(error: unknown): void {
  malformedVideoFrames += 1;
  const waiting = document.querySelector<HTMLElement>("[data-remote-video-starting] p");
  if (waiting) {
    waiting.textContent = "检测到异常视频帧，正在请求新的关键帧恢复画面。";
  }
  if (malformedVideoFrames === 1) {
    void requestControllerKeyframe().catch(() => {});
  }
  if (malformedVideoFrames === 1 || malformedVideoFrames % 60 === 0) {
    console.error("DeskLink video delivery error", error);
  }
}

function updateVideoStartingMessage(): void {
  if (decodedFrames > 0) {
    return;
  }
  const waiting = document.querySelector<HTMLElement>("[data-remote-video-starting] p");
  if (!waiting) {
    return;
  }
  if (relayCompletedFrames > 0 && receivedVideoFrames === 0) {
    waiting.textContent = "中继已收到远程画面，正在交付给本机显示组件。";
  } else if (receivedVideoFrames > 0 && submittedVideoFrames === 0) {
    waiting.textContent = "本机已收到视频，正在等待可解码的关键帧。";
  } else if (submittedVideoFrames > 0) {
    waiting.textContent = decoderPreference === "hardware"
      ? "视频已进入硬件解码器，正在生成第一帧画面。"
      : "视频已进入兼容解码器，正在生成第一帧画面。";
  }
}

function startVideoDecoder(
  canvas: HTMLCanvasElement,
  context: CanvasRenderingContext2D,
  configKey: string,
  preference: "hardware" | "software",
): void {
  releaseVideoDecoder();
  const generation = decoderGeneration;
  decoderPreference = preference;
  decoderRenderedBaseline = decodedFrames;
  decoderSubmittedSinceStart = 0;
  awaitingDecoderKeyframe = true;
  let nextDecoder: VideoDecoder;
  try {
    nextDecoder = new VideoDecoder({
      output: (frame) => {
        if (generation !== decoderGeneration || decoder !== nextDecoder) {
          frame.close();
          return;
        }
        clearDecoderStallTimer();
        pendingVideoFrame?.close();
        pendingVideoFrame = frame;
        if (videoPaintFrame === null) {
          videoPaintFrame = window.requestAnimationFrame(() => {
            videoPaintFrame = null;
            const nextFrame = pendingVideoFrame;
            pendingVideoFrame = null;
            if (!nextFrame || generation !== decoderGeneration) {
              nextFrame?.close();
              return;
            }
            try {
              if (
                nextFrame.displayWidth === canvas.width
                && nextFrame.displayHeight === canvas.height
              ) {
                context.drawImage(nextFrame, 0, 0);
              } else {
                context.drawImage(nextFrame, 0, 0, canvas.width, canvas.height);
              }
              if (decodedFrames === 0 && videoConfigReceivedAtMs !== null) {
                firstFrameMs = Math.max(0, Date.now() - videoConfigReceivedAtMs);
              }
              decodedFrames += 1;
              consecutiveDecoderStalls = 0;
              if (decodedFrames === 1) {
                const waiting = document.querySelector<HTMLElement>("[data-remote-video-starting]");
                if (waiting) {
                  waiting.hidden = true;
                }
              }
              if (decodedFrames === 1) {
                reportRenderMetrics(true);
              }
            } catch {
              fallbackOrFailVideo(configKey, preference);
            } finally {
              nextFrame.close();
            }
          });
        }
      },
      error: () => {
        if (generation === decoderGeneration && decoder === nextDecoder) {
          fallbackOrFailVideo(configKey, preference);
        }
      },
    });
    decoder = nextDecoder;
    nextDecoder.configure({
      codec: h264CodecFromSequenceHeader(new Uint8Array(videoConfig?.sequenceHeader ?? [])),
      codedWidth: videoConfig?.width ?? canvas.width,
      codedHeight: videoConfig?.height ?? canvas.height,
      hardwareAcceleration: preference === "hardware" ? "prefer-hardware" : "prefer-software",
      optimizeForLatency: true,
    });
  } catch {
    fallbackOrFailVideo(configKey, preference);
    return;
  }
  const waiting = document.querySelector<HTMLElement>("[data-remote-video-starting] p");
  if (waiting) {
    waiting.textContent = preference === "hardware"
      ? "正在接收并解码第一个加密视频帧。"
      : "正在使用兼容解码模式恢复远程画面。";
  }
  if (pendingVideoKeyframe) {
    const pending = pendingVideoKeyframe;
    pendingVideoKeyframe = null;
    submitVideoChunk(pending);
  }
  void requestControllerKeyframe().catch(showOperationError);
}

function armDecoderStallWatch(): void {
  if (decoderStallTimer !== null || decoderSubmittedSinceStart === 0) {
    return;
  }
  const generation = decoderGeneration;
  const configKey = videoConfig ? videoConfigKey(videoConfig) : "";
  const preference = decoderPreference;
  // Compare against the frame count at every arm. Keeping the original
  // session baseline would only detect a stall before the first frame.
  decoderRenderedBaseline = decodedFrames;
  decoderStallTimer = window.setTimeout(() => {
    decoderStallTimer = null;
    if (
      generation === decoderGeneration
      && decodedFrames === decoderRenderedBaseline
      && decoderSubmittedSinceStart > 0
    ) {
      fallbackOrFailVideo(configKey, preference);
    }
  }, preference === "hardware" ? 1_500 : 3_000);
}

function fallbackOrFailVideo(configKey: string, preference: "hardware" | "software"): void {
  if (!videoConfig || videoConfigKey(videoConfig) !== configKey) {
    return;
  }
  if (decoderPreference !== preference) {
    return;
  }
  decoderRecoveries += 1;
  consecutiveDecoderStalls += 1;
  reportRenderMetrics(true);
  if (preference === "hardware") {
    const canvas = document.querySelector<HTMLCanvasElement>("[data-remote-canvas]");
      const context = canvas?.getContext("2d", { alpha: false, desynchronized: true });
    if (canvas && context) {
      decoderPreference = "software";
      queueMicrotask(() => startVideoDecoder(canvas, context, configKey, "software"));
      return;
    }
  }
  if (consecutiveDecoderStalls <= 2) {
    const canvas = document.querySelector<HTMLCanvasElement>("[data-remote-canvas]");
    const context = canvas?.getContext("2d", { alpha: false, desynchronized: true });
    if (canvas && context) {
      releaseVideoDecoder();
      queueMicrotask(() => startVideoDecoder(canvas, context, configKey, "software"));
      return;
    }
  }
  failedVideoConfig = configKey;
  feedback = {
    tone: "error",
    message: receivedVideoFrames > 0
      ? "已经收到远程视频，但 WebView2 无法显示画面。请更新 Microsoft Edge WebView2 Runtime 后重试。"
      : "尚未收到远程视频画面，请刷新画面或重新连接主机。",
  };
  queueMicrotask(requestRender);
}

function reportRenderMetrics(force = false): void {
  if (!videoConfig) {
    return;
  }
  const now = Date.now();
  if (!force && now - lastRenderMetricsReportedAtMs < 10_000) {
    return;
  }
  lastRenderMetricsReportedAtMs = now;
  void reportControllerRenderMetrics({
    streamId: videoConfig.streamId,
    receivedFrames: receivedVideoFrames,
    submittedFrames: submittedVideoFrames,
    displayedFrames: decodedFrames,
    malformedFrames: malformedVideoFrames,
    decoderRecoveries,
    videoPullFailures,
    firstFrameMs,
  }).catch(() => {
    // Diagnostics must never interrupt a live remote-control session.
  });
}

function clearDecoderStallTimer(): void {
  if (decoderStallTimer !== null) {
    window.clearTimeout(decoderStallTimer);
    decoderStallTimer = null;
  }
}

function releaseVideoDecoder(): void {
  decoderGeneration += 1;
  clearDecoderStallTimer();
  if (videoPaintFrame !== null) {
    window.cancelAnimationFrame(videoPaintFrame);
    videoPaintFrame = null;
  }
  pendingVideoFrame?.close();
  pendingVideoFrame = null;
  if (decoder && decoder.state !== "closed") {
    decoder.close();
  }
  decoder = null;
}

function retryVideo(): void {
  failedVideoConfig = null;
  feedback = null;
  requestRender();
}

async function changeRemoteDisplay(displayId: number): Promise<void> {
  if (!remoteDisplaySwitch.begin(
    displayId,
    activeRemoteDisplayId,
    remoteDisplays.map((display) => display.id),
  )) {
    return;
  }
  releaseInputState();
  inputDispatcher.discardPendingMoves();
  if (pointerFrame !== null) {
    window.cancelAnimationFrame(pointerFrame);
    pointerFrame = null;
  }
  remoteCanvasBounds = null;
  remoteViewportBounds = null;
  remoteDisplaySwitchStatus = { tone: "pending", message: "正在切换远程屏幕…" };
  updateRemoteDisplayControls();
  clearRemoteDisplaySwitchTimer();
  remoteDisplaySwitchTimer = window.setTimeout(() => {
    remoteDisplaySwitchTimer = null;
    if (remoteDisplaySwitch.fail(displayId)) {
      remoteDisplaySwitchStatus = {
        tone: "error",
        message: "屏幕切换超时，仍保持当前屏幕。可以重新选择。",
      };
      updateRemoteDisplayControls();
    }
  }, 8_000);
  try {
    await selectControllerDisplay(displayId);
  } catch (error) {
    if (remoteDisplaySwitch.fail(displayId)) {
      clearRemoteDisplaySwitchTimer();
      remoteDisplaySwitchStatus = {
        tone: "error",
        message: `无法切换远程屏幕：${normalizeError(error)}`,
      };
      updateRemoteDisplayControls();
    }
  }
}

async function changeVideoQuality(preference: VideoQualityPreference): Promise<void> {
  if (
    pendingVideoQuality !== null
    || preference === videoQualityPreference
    || !["automatic", "smooth", "balanced", "sharp"].includes(preference)
  ) {
    return;
  }
  pendingVideoQuality = preference;
  const picker = document.querySelector<HTMLSelectElement>("[data-controller-video-quality]");
  if (picker) {
    picker.disabled = true;
    picker.value = preference;
  }
  clearVideoQualityAckTimer();
  videoQualityAckTimer = window.setTimeout(() => {
    if (pendingVideoQuality !== preference) {
      return;
    }
    pendingVideoQuality = null;
    const currentPicker = document.querySelector<HTMLSelectElement>("[data-controller-video-quality]");
    if (currentPicker) {
      currentPicker.disabled = false;
      currentPicker.value = videoQualityPreference;
      currentPicker.setCustomValidity("目标电脑未确认新的画质档位，请稍后重试。");
      currentPicker.reportValidity();
      currentPicker.setCustomValidity("");
    }
    videoQualityAckTimer = null;
  }, 8_000);
  try {
    await setControllerVideoQuality(preference);
  } catch (error) {
    clearVideoQualityAckTimer();
    pendingVideoQuality = null;
    if (picker?.isConnected) {
      picker.disabled = false;
      picker.value = videoQualityPreference;
      picker.setCustomValidity(normalizeError(error));
      picker.reportValidity();
      picker.setCustomValidity("");
    }
  }
}

function videoQualityPresetLabel(preset: VideoQualityPreset): string {
  switch (preset) {
    case "smooth":
      return "流畅";
    case "balanced":
      return "均衡";
    case "sharp":
      return "清晰";
  }
}

function updateRemoteSessionSummary(): void {
  const element = document.querySelector<HTMLElement>("[data-controller-metrics]");
  if (!element || !videoConfig) {
    return;
  }
  element.textContent = remoteSessionSummary(
    videoConfig.width,
    videoConfig.height,
    videoQualityPreference,
    appliedVideoQuality,
  );
}

function changeRemoteScaleMode(value: string): void {
  if (value !== "fit" && value !== "actual") {
    const picker = document.querySelector<HTMLSelectElement>("[data-controller-scale]");
    if (picker) {
      picker.value = remoteScaleMode;
    }
    return;
  }
  const mode = normalizeRemoteScaleMode(value);
  if (mode === remoteScaleMode) {
    return;
  }
  remoteScaleMode = mode;
  saveRemoteScaleMode(mode);
  const viewport = document.querySelector<HTMLElement>("[data-remote-viewport]");
  const canvas = document.querySelector<HTMLCanvasElement>("[data-remote-canvas]");
  if (!viewport) {
    return;
  }
  viewport.dataset.scaleMode = mode;
  remoteCanvasBounds = null;
  remoteViewportBounds = null;
  if (mode === "fit") {
    setRemotePanMode(false);
  } else {
    syncRemoteInteractionControls();
  }
  if (!canvas) {
    return;
  }
  if (mode === "fit") {
    viewport.scrollLeft = 0;
    viewport.scrollTop = 0;
  }
  scheduleRemoteScaleLayout(viewport, canvas, mode === "actual");
}

function setRemotePanMode(enabled: boolean): void {
  const next = enabled && remoteScaleMode === "actual";
  if (next !== remotePanMode) {
    releaseInputState();
    inputDispatcher.discardPendingMoves();
    if (pointerFrame !== null) {
      window.cancelAnimationFrame(pointerFrame);
      pointerFrame = null;
    }
    remotePanMode = next;
  }
  syncRemoteInteractionControls();
}

function syncRemoteInteractionControls(): void {
  const available = remoteScaleMode === "actual";
  if (!available) {
    remotePanMode = false;
  }
  const button = document.querySelector<HTMLButtonElement>("[data-controller-pan]");
  const viewport = document.querySelector<HTMLElement>("[data-remote-viewport]");
  const hint = document.querySelector<HTMLElement>("[data-remote-focus-hint]");
  if (button) {
    button.disabled = !available;
    button.classList.toggle("toolbar-button--active", remotePanMode);
    button.setAttribute("aria-pressed", String(remotePanMode));
    button.title = available
      ? remotePanMode
        ? "恢复向远程电脑发送鼠标和键盘输入"
        : "只在本地拖动或滚动画面，不会操作远程电脑"
      : "切换到 1:1 后可以浏览超出窗口的画面";
    button.innerHTML = `${icon("hand")}${remotePanMode ? "继续控制" : "浏览画面"}`;
    renderLucideIcons(button);
  }
  if (viewport) {
    viewport.dataset.interactionMode = remotePanMode ? "pan" : "control";
    viewport.setAttribute(
      "aria-label",
      remotePanMode
        ? "远程 Windows 桌面浏览模式。拖动画面或使用滚轮查看，点击继续控制可恢复远程输入。"
        : "远程 Windows 桌面，点击后可发送键盘和鼠标输入。",
    );
  }
  if (hint) {
    hint.textContent = remotePanMode
      ? "浏览模式：拖动画面或滚轮查看，不会操作远程电脑"
      : "点击画面开始控制 · Ctrl+Alt+Delete 必须在主机本地操作";
  }
}

function scheduleRemoteScaleLayout(
  viewport: HTMLElement,
  canvas: HTMLCanvasElement,
  center: boolean,
): void {
  if (remoteScaleFrame !== null) {
    window.cancelAnimationFrame(remoteScaleFrame);
  }
  remoteScaleFrame = window.requestAnimationFrame(() => {
    remoteScaleFrame = null;
    if (!viewport.isConnected || !canvas.isConnected) {
      return;
    }
    if (center) {
      viewport.scrollLeft = Math.max(0, (viewport.scrollWidth - viewport.clientWidth) / 2);
      viewport.scrollTop = Math.max(0, (viewport.scrollHeight - viewport.clientHeight) / 2);
    }
    remoteCanvasBounds = readRemoteCanvasBounds(canvas);
    remoteViewportBounds = viewport.getBoundingClientRect();
  });
}

function clearVideoQualityAckTimer(): void {
  if (videoQualityAckTimer !== null) {
    window.clearTimeout(videoQualityAckTimer);
    videoQualityAckTimer = null;
  }
}

function clearRemoteDisplaySwitchTimer(): void {
  if (remoteDisplaySwitchTimer !== null) {
    window.clearTimeout(remoteDisplaySwitchTimer);
    remoteDisplaySwitchTimer = null;
  }
}

function clearRemoteDisplaySwitch(): void {
  clearRemoteDisplaySwitchTimer();
  remoteDisplaySwitch.reset();
  remoteDisplaySwitchStatus = null;
}

function updateRemoteDisplayControls(): void {
  const picker = document.querySelector<HTMLSelectElement>("[data-controller-display]");
  if (picker) {
    picker.disabled = remoteDisplaySwitch.pendingId !== null;
    picker.value = String(remoteDisplaySwitch.pendingId ?? activeRemoteDisplayId ?? "");
  }
  const status = document.querySelector<HTMLElement>("[data-controller-display-status]");
  if (status) {
    status.hidden = remoteDisplaySwitchStatus === null;
    status.dataset.tone = remoteDisplaySwitchStatus?.tone ?? "";
    status.textContent = remoteDisplaySwitchStatus?.message ?? "";
  }
}

const pressedKeys = new RemoteKeyboardState();
const pressedButtons = new Set<"left" | "right" | "middle">();

function bindRemoteInput(viewport: HTMLElement, canvas: HTMLCanvasElement): void {
  let pendingPoint: { x: number; y: number } | null = null;
  let panDrag: (RemotePanOrigin & { pointerId: number }) | null = null;
  const sendPendingPoint = () => {
    if (pointerFrame !== null) {
      window.cancelAnimationFrame(pointerFrame);
    }
    pointerFrame = null;
    if (pendingPoint) {
      fireInput({ kind: "mouseMove", ...pendingPoint });
      pendingPoint = null;
    }
  };
  const discardPendingPoint = () => {
    if (pointerFrame !== null) {
      window.cancelAnimationFrame(pointerFrame);
      pointerFrame = null;
    }
    pendingPoint = null;
  };
  const finishPanDrag = () => {
    if (panDrag && viewport.hasPointerCapture(panDrag.pointerId)) {
      viewport.releasePointerCapture(panDrag.pointerId);
    }
    panDrag = null;
    delete viewport.dataset.panning;
  };
  viewport.addEventListener("pointermove", (event) => {
    if (!pointerInsideViewport) {
      pointerInsideViewport = true;
      const cursor = document.querySelector<HTMLElement>("[data-remote-cursor]");
      if (cursor) {
        cursor.hidden = true;
      }
    }
    if (remotePanMode) {
      discardPendingPoint();
      if (panDrag?.pointerId === event.pointerId) {
        event.preventDefault();
        const position = remotePanPosition(panDrag, event.clientX, event.clientY, {
          clientWidth: viewport.clientWidth,
          clientHeight: viewport.clientHeight,
          scrollWidth: viewport.scrollWidth,
          scrollHeight: viewport.scrollHeight,
        });
        viewport.scrollLeft = position.left;
        viewport.scrollTop = position.top;
      }
      return;
    }
    const point = pointerPosition(event, canvas);
    if (!point) {
      return;
    }
    pendingPoint = point;
    if (pointerFrame === null) {
      pointerFrame = window.requestAnimationFrame(sendPendingPoint);
    }
  });
  viewport.addEventListener("pointerenter", () => {
    pointerInsideViewport = true;
    const cursor = document.querySelector<HTMLElement>("[data-remote-cursor]");
    if (cursor) {
      cursor.hidden = true;
    }
  });
  viewport.addEventListener("pointerleave", () => {
    pointerInsideViewport = false;
  });
  viewport.addEventListener("pointerdown", (event) => {
    if (remotePanMode) {
      discardPendingPoint();
      if (event.button !== 0) {
        return;
      }
      event.preventDefault();
      viewport.focus({ preventScroll: true });
      panDrag = {
        pointerId: event.pointerId,
        clientX: event.clientX,
        clientY: event.clientY,
        scrollLeft: viewport.scrollLeft,
        scrollTop: viewport.scrollTop,
      };
      viewport.dataset.panning = "true";
      viewport.setPointerCapture(event.pointerId);
      return;
    }
    const button = mouseButton(event.button);
    if (!button) {
      return;
    }
    remoteCanvasBounds = readRemoteCanvasBounds(canvas);
    remoteViewportBounds = viewport.getBoundingClientRect();
    const point = pointerPosition(event, canvas);
    if (!point) {
      return;
    }
    event.preventDefault();
    viewport.focus({ preventScroll: true });
    viewport.setPointerCapture(event.pointerId);
    pendingPoint = point;
    sendPendingPoint();
    pressedButtons.add(button);
    fireInput({ kind: "mouseButton", button, pressed: true });
  });
  viewport.addEventListener("pointerup", (event) => {
    if (panDrag?.pointerId === event.pointerId) {
      event.preventDefault();
      finishPanDrag();
      return;
    }
    if (remotePanMode) {
      return;
    }
    const button = mouseButton(event.button);
    if (!button) {
      return;
    }
    event.preventDefault();
    const point = pointerPosition(event, canvas);
    if (point) {
      pendingPoint = point;
      sendPendingPoint();
    }
    pressedButtons.delete(button);
    fireInput({ kind: "mouseButton", button, pressed: false });
  });
  const releaseCapturedInput = () => {
    finishPanDrag();
    sendPendingPoint();
    releaseInputState();
  };
  viewport.addEventListener("pointercancel", releaseCapturedInput);
  viewport.addEventListener("lostpointercapture", releaseCapturedInput);
  viewport.addEventListener("contextmenu", (event) => event.preventDefault());
  viewport.addEventListener("wheel", (event) => {
    if (remotePanMode) {
      discardPendingPoint();
      return;
    }
    event.preventDefault();
    sendPendingPoint();
    const deltaX = clampWheel(Math.round(event.deltaX));
    const deltaY = clampWheel(-Math.round(event.deltaY));
    if (deltaX !== 0 || deltaY !== 0) {
      fireInput({ kind: "wheel", deltaX, deltaY });
    }
  }, { passive: false });
  viewport.addEventListener("keydown", (event) => {
    if (remotePanMode) {
      if (event.key === "Escape") {
        event.preventDefault();
        finishPanDrag();
        setRemotePanMode(false);
      }
      return;
    }
    if (smartPasteBusy) {
      event.preventDefault();
      return;
    }
    if (isRemoteClipboardPasteShortcut(event)) {
      event.preventDefault();
      event.stopPropagation();
      void pasteLocalClipboardText();
      return;
    }
    sendKeyboardEvent(event, true);
  });
  viewport.addEventListener("keyup", (event) => {
    if (!remotePanMode) {
      sendKeyboardEvent(event, false);
    }
  });
  viewport.addEventListener("blur", releaseCapturedInput);
}

function sendKeyboardEvent(event: KeyboardEvent, pressed: boolean): void {
  if (event.repeat) {
    return;
  }
  const key = keyboardKey(event.key);
  if (!key) {
    return;
  }
  event.preventDefault();
  const physicalCode = event.code || key.key;
  if (pressed) {
    const ownModifier = keyboardModifierMask(key.key);
    const input: ControllerKeyInput = {
      kind: "key",
      key: key.key,
      character: key.character,
      pressed: true,
      modifiers: keyboardModifiers(event, pressedKeys.modifierMask() | ownModifier),
    };
    for (const next of pressedKeys.press(physicalCode, input)) {
      fireInput(next);
    }
  } else {
    for (const next of pressedKeys.release(physicalCode)) {
      fireInput(next);
    }
  }
}

function releaseInputState(): void {
  for (const input of pressedKeys.releaseAll()) {
    fireInput(input);
  }
  for (const button of pressedButtons) {
    fireInput({ kind: "mouseButton", button, pressed: false });
  }
  pressedButtons.clear();
}

async function pasteLocalClipboardText(): Promise<void> {
  if (smartPasteBusy || snapshot?.runtime.state !== "connected") {
    return;
  }
  smartPasteBusy = true;
  smartPastePending = true;
  clipboardTransfer = {
    kind: "clipboard",
    state: "sending",
    operation: "paste",
    message: "正在将本机剪贴板文字粘贴到远程电脑…",
  };
  updateTransferPanel({ ledger: false });
  inputDispatcher.discardPendingMoves();
  releaseInputState();
  try {
    await inputDispatcher.drain();
    await pasteControllerClipboardText();
  } catch (error) {
    smartPasteBusy = false;
    smartPastePending = false;
    openTransferPanel();
    clipboardTransfer = {
      kind: "clipboard",
      state: "failed",
      operation: "paste",
      message: normalizeError(error),
    };
  } finally {
    updateTransferPanel({ ledger: false });
    document.querySelector<HTMLElement>("[data-remote-viewport]")?.focus({ preventScroll: true });
  }
}

function fireInput(input: ControllerInput): void {
  const streamId = snapshot?.runtime.state === "connected"
    ? snapshot.runtime.streamId
    : null;
  if (streamId !== null) {
    inputDispatcher.enqueue(input, streamId);
  }
}

function sendRemoteKeyTap(key: string): void {
  fireInput({ kind: "key", key, pressed: true, modifiers: 0 });
  fireInput({ kind: "key", key, pressed: false, modifiers: 0 });
  document.querySelector<HTMLElement>("[data-remote-viewport]")?.focus({ preventScroll: true });
}

async function submitTextInput(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  if (textSending) {
    return;
  }
  const input = document.querySelector<HTMLInputElement>("[data-controller-text-input]");
  if (!input || !input.reportValidity()) {
    return;
  }
  const text = input.value;
  const submit = (event.currentTarget as HTMLFormElement).querySelector<HTMLButtonElement>(
    'button[type="submit"]',
  );
  textSending = true;
  input.disabled = true;
  if (submit) {
    submit.disabled = true;
    submit.setAttribute("aria-busy", "true");
  }
  try {
    await sendControllerText(text);
    input.value = "";
    closeTextInput();
  } catch (error) {
    showOperationError(error);
  } finally {
    textSending = false;
    if (input.isConnected) {
      input.disabled = false;
    }
    if (submit?.isConnected) {
      submit.disabled = false;
      submit.removeAttribute("aria-busy");
    }
  }
}

function closeTextInput(): void {
  const panel = document.querySelector<HTMLElement>("[data-controller-text-panel]");
  const viewport = document.querySelector<HTMLElement>("[data-remote-viewport]");
  if (panel) {
    panel.hidden = true;
  }
  revealRemoteToolbar();
  viewport?.focus({ preventScroll: true });
}

function pointerPosition(event: PointerEvent, canvas: HTMLCanvasElement): { x: number; y: number } | null {
  const bounds = remoteCanvasBounds ?? readRemoteCanvasBounds(canvas);
  return normalizedPointerPosition(event.clientX, event.clientY, bounds);
}

function updateRemoteCursor(x: number, y: number, visible: boolean): void {
  const cursor = remoteCursorElement;
  const canvas = remoteCanvasElement;
  const viewport = remoteViewportElement;
  if (!cursor || !canvas || !viewport) {
    return;
  }
  if (pointerInsideViewport) {
    cursor.hidden = true;
    return;
  }
  const canvasBounds = remoteCanvasBounds ?? readRemoteCanvasBounds(canvas);
  const viewportBounds = remoteViewportBounds ?? viewport.getBoundingClientRect();
  const position = remoteCursorContentPosition(
    x,
    y,
    canvasBounds,
    viewportBounds,
    viewport.scrollLeft,
    viewport.scrollTop,
  );
  const left = position.left;
  const top = position.top;
  cursor.style.transform = `translate3d(${left - 1}px, ${top - 1}px, 0)`;
  cursor.hidden = !visible;
}

function setupRemoteGeometry(viewport: HTMLElement, canvas: HTMLCanvasElement): void {
  remoteViewportElement = viewport;
  remoteCanvasElement = canvas;
  remoteCursorElement = document.querySelector<HTMLElement>("[data-remote-cursor]");
  const refresh = () => {
    remoteCanvasBounds = readRemoteCanvasBounds(canvas);
    remoteViewportBounds = viewport.getBoundingClientRect();
  };
  refresh();
  remoteResizeObserver?.disconnect();
  remoteResizeObserver = new ResizeObserver(refresh);
  remoteResizeObserver.observe(viewport);
  remoteResizeObserver.observe(canvas);
  viewport.addEventListener("scroll", refresh, { passive: true });
}

function readRemoteCanvasBounds(canvas: HTMLCanvasElement): PointerBounds {
  const bounds = canvas.getBoundingClientRect();
  return remoteScaleMode === "fit"
    ? containedPointerBounds(bounds, canvas.width, canvas.height)
    : bounds;
}

function clearRemoteToolbarTimer(): void {
  if (remoteToolbarTimer !== null) {
    window.clearTimeout(remoteToolbarTimer);
    remoteToolbarTimer = null;
  }
}

function remoteToolbarVisibilityInput(nowMs: number): RemoteToolbarVisibilityInput {
  const textPanel = document.querySelector<HTMLElement>("[data-controller-text-panel]");
  return {
    connected: snapshot?.runtime.state === "connected",
    fullscreen: remoteFullscreenActive,
    nowMs,
    lastRevealedAtMs: remoteToolbarLastRevealedAtMs,
    pointerNearTop: remoteToolbarPointerNearTop,
    toolbarFocused: remoteToolbarFocused,
    panelOpen: transferPanelOpen || (textPanel !== null && !textPanel.hidden),
  };
}

function updateRemoteToolbarVisibility(nowMs = Date.now()): void {
  clearRemoteToolbarTimer();
  const session = document.querySelector<HTMLElement>(".remote-session");
  if (!session) {
    return;
  }
  const input = remoteToolbarVisibilityInput(nowMs);
  const visible = remoteToolbarVisible(input);
  session.dataset.remoteToolbarVisible = String(visible);
  const delay = remoteToolbarHideDelay(input);
  if (visible && delay !== null && delay > 0) {
    remoteToolbarTimer = window.setTimeout(() => {
      remoteToolbarTimer = null;
      updateRemoteToolbarVisibility();
    }, delay + 16);
  }
}

function revealRemoteToolbar(nowMs = Date.now()): void {
  remoteToolbarLastRevealedAtMs = nowMs;
  updateRemoteToolbarVisibility(nowMs);
}

function resetRemoteToolbarState(): void {
  clearRemoteToolbarTimer();
  remoteToolbarLastRevealedAtMs = 0;
  remoteToolbarPointerNearTop = false;
  remoteToolbarFocused = false;
  const session = document.querySelector<HTMLElement>(".remote-session");
  if (session) {
    session.dataset.remoteToolbarVisible = "true";
  }
}

function handleRemoteToolbarPointerMove(event: PointerEvent): void {
  if (!remoteFullscreenActive) {
    return;
  }
  const toolbarHovered = document.querySelector<HTMLElement>(".remote-toolbar")?.matches(":hover") === true;
  const pointerNearTop = event.clientY <= 12 || toolbarHovered;
  if (pointerNearTop === remoteToolbarPointerNearTop) {
    return;
  }
  remoteToolbarPointerNearTop = pointerNearTop;
  revealRemoteToolbar();
}

async function toggleFullscreen(): Promise<void> {
  if (!document.querySelector("[data-remote-viewport]")) {
    return;
  }
  await setRemoteFullscreen(!remoteFullscreenDesired, true);
  if (remoteFullscreenActive) {
    document.querySelector<HTMLElement>("[data-remote-viewport]")?.focus({ preventScroll: true });
  }
}

function setRemoteFullscreen(active: boolean, reportErrors: boolean): Promise<void> {
  remoteFullscreenDesired = active;
  if (!remoteFullscreenOperation) {
    remoteFullscreenOperation = applyRemoteFullscreenRequests(reportErrors).finally(() => {
      remoteFullscreenOperation = null;
      if (remoteFullscreenDesired !== remoteFullscreenActive) {
        void setRemoteFullscreen(remoteFullscreenDesired, false);
      }
    });
  }
  return remoteFullscreenOperation;
}

async function applyRemoteFullscreenRequests(reportErrors: boolean): Promise<void> {
  remoteFullscreenBusy = true;
  syncRemoteFullscreenControl();
  try {
    while (remoteFullscreenActive !== remoteFullscreenDesired) {
      const target = remoteFullscreenDesired;
      releaseInputState();
      inputDispatcher.discardPendingMoves();
      await controllerWindow.setFullscreen(target);
      applyRemoteFullscreenState(target, false);
    }
  } catch (error) {
    remoteFullscreenDesired = remoteFullscreenActive;
    if (reportErrors) {
      feedback = {
        tone: "error",
        message: `Windows 无法${remoteFullscreenDesired ? "退出" : "进入"}全屏，请重新尝试。`,
      };
      requestRender();
    }
  } finally {
    remoteFullscreenBusy = false;
    syncRemoteFullscreenControl();
  }
}

function applyRemoteFullscreenState(active: boolean, synchronizeDesired = true): void {
  remoteFullscreenActive = active;
  if (synchronizeDesired) {
    remoteFullscreenDesired = active;
  }
  if (active) {
    document.documentElement.dataset.remoteFullscreen = "true";
    revealRemoteToolbar();
  } else {
    delete document.documentElement.dataset.remoteFullscreen;
    resetRemoteToolbarState();
  }
  remoteCanvasBounds = null;
  remoteViewportBounds = null;
  syncRemoteFullscreenControl();
}

async function synchronizeRemoteFullscreen(): Promise<void> {
  if (remoteFullscreenBusy) {
    return;
  }
  try {
    const active = await controllerWindow.isFullscreen();
    if (!remoteFullscreenBusy) {
      applyRemoteFullscreenState(active);
    }
  } catch {
    // The UI remains usable in browser preview and on older embedded runtimes.
  }
}

function scheduleRemoteFullscreenSync(): void {
  if (remoteFullscreenResizeTimer !== null) {
    window.clearTimeout(remoteFullscreenResizeTimer);
  }
  remoteFullscreenResizeTimer = window.setTimeout(() => {
    remoteFullscreenResizeTimer = null;
    void synchronizeRemoteFullscreen();
  }, 80);
}

function handleRemoteFullscreenEscape(event: KeyboardEvent): void {
  if (!remoteFullscreenActive || event.key !== "Escape" || event.repeat) {
    return;
  }
  event.preventDefault();
  event.stopImmediatePropagation();
  void setRemoteFullscreen(false, true);
}

function syncRemoteFullscreenControl(): void {
  const button = document.querySelector<HTMLButtonElement>("[data-controller-fullscreen]");
  if (!button) {
    return;
  }
  button.disabled = remoteFullscreenBusy;
  button.setAttribute("aria-busy", String(remoteFullscreenBusy));
  button.setAttribute("aria-pressed", String(remoteFullscreenActive));
  button.setAttribute("aria-label", remoteFullscreenActive ? "退出全屏" : "进入全屏");
  button.title = remoteFullscreenActive ? "退出全屏（Esc）" : "进入全屏";
  button.innerHTML = `${icon(remoteFullscreenActive ? "minimize-2" : "maximize-2")}<span data-controller-fullscreen-label>${remoteFullscreenActive ? "退出全屏" : "全屏"}</span>`;
  renderLucideIcons(button);
}

function toUint8Array(payload: ControllerVideoPayload): Uint8Array {
  if (payload instanceof Uint8Array) {
    return payload;
  }
  if (ArrayBuffer.isView(payload)) {
    return new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength);
  }
  if (payload instanceof ArrayBuffer) {
    return new Uint8Array(payload);
  }
  if (Array.isArray(payload)) {
    return Uint8Array.from(payload);
  }
  throw new TypeError("Tauri 返回了无法识别的视频二进制格式");
}

function showOperationError(error: unknown): void {
  feedback = { tone: "error", message: normalizeError(error) };
  requestRender();
}

function normalizeError(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return "DeskLink 无法完成此控制端操作。";
}

window.setInterval(updateConnectionProgressClock, 1000);
