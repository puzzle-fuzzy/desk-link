import {
  clearSavedDevices,
  connectDevice,
  connectSavedDevice,
  createControllerChannels,
  disconnectController,
  forgetSavedDevice,
  getControllerSnapshot,
  reconnectController,
  renameSavedDevice,
  reportControllerRenderMetrics,
  requestControllerKeyframe,
  selectControllerDisplay,
  sendControllerInput,
  sendControllerText,
} from "./api";
import type { ControllerChannels } from "./api";
import {
  deviceCredentialsAreValid,
  formatDeviceId,
  normalizeTemporaryPassword,
} from "./device-credentials";
import { escapeHtml } from "./html";
import {
  MAX_POINTER_COORDINATE,
  clampWheel,
  keyboardKey,
  keyboardModifiers,
  mouseButton,
  normalizedPointerPosition,
} from "./remote-input";
import type {
  ControllerInput,
  SavedControllerConnectionSummary,
  SavedDeviceCredentialSummary,
  ControllerSignal,
  ControllerSnapshot,
  ControllerVideoConfigSignal,
  RemoteDisplaySummary,
} from "./types";
import { h264CodecFromSequenceHeader, videoConfigKey } from "./video-config";
import { deviceIdsMatch, formatLastUsed } from "./saved-device";
import { RemoteInputDispatcher } from "./remote-input-dispatcher";
import {
  CONNECTION_PROGRESS_STEPS,
  connectionProgressPresentation,
  formatConnectionElapsed,
} from "./connection-progress";
import { icon } from "./icons";
import { isH264Keyframe, prepareH264AccessUnit } from "./h264-annex-b";

type RenderRequest = () => void;
type ControllerFeedback = { tone: "success" | "error" | "info"; message: string } | null;
type VideoPayload = ArrayBuffer | ArrayBufferView | number[];

const FRAME_PREFIX_BYTES = 17;
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
let consecutiveDecoderStalls = 0;
let videoConfigReceivedAtMs: number | null = null;
let firstFrameMs: number | null = null;
let lastRenderMetricsReportedAtMs = 0;
let pointerFrame: number | null = null;
let remoteResizeObserver: ResizeObserver | null = null;
let remoteCanvasBounds: DOMRectReadOnly | null = null;
let remoteViewportBounds: DOMRectReadOnly | null = null;
let pointerInsideViewport = false;
let requestRender: RenderRequest = () => {};
let decodedFrames = 0;
let textSending = false;
let failedVideoConfig: string | null = null;
let remoteDisplays: RemoteDisplaySummary[] = [];
let activeRemoteDisplayId: number | null = null;
let pendingRemoteDisplayId: number | null = null;
let attemptStartedAtMs: number | null = null;
let attemptTarget = "";
let forgetConfirmation: string | null = null;
let renameDeviceId: string | null = null;
let renameDraft = "";
let renameBusy = false;
const inputDispatcher = new RemoteInputDispatcher(sendControllerInput);

function resetVideoTelemetry(): void {
  pendingVideoKeyframe = null;
  receivedVideoFrames = 0;
  submittedVideoFrames = 0;
  relayCompletedFrames = 0;
  malformedVideoFrames = 0;
  decodedFrames = 0;
  decoderRecoveries = 0;
  consecutiveDecoderStalls = 0;
  videoConfigReceivedAtMs = null;
  firstFrameMs = null;
  lastRenderMetricsReportedAtMs = 0;
}

export async function initializeController(renderer: RenderRequest): Promise<void> {
  requestRender = renderer;
  try {
    snapshot = await getControllerSnapshot();
    const latestSavedDevice = snapshot.savedDevices.at(0);
    if (!deviceIdDraft && latestSavedDevice) {
      deviceIdDraft = latestSavedDevice.deviceId;
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
  releaseVideoDecoder();
  remoteResizeObserver?.disconnect();
  remoteResizeObserver = null;
  remoteCanvasBounds = null;
  remoteViewportBounds = null;
  pointerInsideViewport = false;
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
  document.querySelector<HTMLButtonElement>("[data-controller-fullscreen]")?.addEventListener("click", () => {
    void toggleFullscreen();
  });
  document.querySelector<HTMLSelectElement>("[data-controller-display]")?.addEventListener("change", (event) => {
    void changeRemoteDisplay(Number((event.currentTarget as HTMLSelectElement).value));
  });
  document.querySelector<HTMLButtonElement>("[data-controller-text]")?.addEventListener("click", () => {
    const panel = document.querySelector<HTMLElement>("[data-controller-text-panel]");
    const input = document.querySelector<HTMLInputElement>("[data-controller-text-input]");
    if (panel && input) {
      panel.hidden = false;
      input.focus();
    }
  });
  document.querySelector<HTMLButtonElement>("[data-controller-text-cancel]")?.addEventListener("click", () => {
    closeTextInput();
  });
  document.querySelector<HTMLFormElement>("[data-controller-text-form]")?.addEventListener("submit", (event) => {
    void submitTextInput(event);
  });
  if (snapshot?.runtime.state === "connected") {
    setupRemoteDesktop();
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

function renderRemoteDesktop(): string {
  const config = videoConfig;
  const videoFailed = config ? failedVideoConfig === videoConfigKey(config) : false;
  return `
    <section class="remote-session" aria-label="当前远程控制会话">
      <div class="remote-toolbar">
        <div class="remote-toolbar-status">
          <span class="remote-live-dot" aria-hidden="true"></span>
          <div><strong>实时远程桌面</strong><small data-controller-metrics>${config ? `${config.width} × ${config.height} · 已加密` : "正在等待首个视频画面"}</small></div>
        </div>
        <div class="remote-toolbar-actions">
          ${remoteDisplays.length > 1 ? `
            <label class="remote-display-picker" title="切换目标电脑的显示器">
              ${icon("monitor")}
              <span class="sr-only">选择远程显示器</span>
              <select data-controller-display aria-label="选择远程显示器" ${pendingRemoteDisplayId === null ? "" : "disabled"}>
                ${remoteDisplays.map((display, index) => `<option value="${display.id}" ${display.id === (pendingRemoteDisplayId ?? activeRemoteDisplayId) ? "selected" : ""}>屏幕 ${index + 1}${display.primary ? "（主屏）" : ""} · ${display.width} × ${display.height}</option>`).join("")}
              </select>
            </label>` : ""}
          <button class="toolbar-button" type="button" data-controller-text title="发送中文、符号或一段文字">${icon("keyboard")}发送文字</button>
          <button class="toolbar-button" type="button" data-controller-keyframe title="刷新远程画面">${icon("refresh-cw")}刷新画面</button>
          <button class="toolbar-button" type="button" data-controller-fullscreen>${icon("maximize-2")}全屏</button>
          <button class="toolbar-button toolbar-button--danger" type="button" data-controller-disconnect>${icon("log-out")}断开连接</button>
        </div>
      </div>
      <form class="remote-text-entry" data-controller-text-form data-controller-text-panel hidden>
        <label for="remote-text-input">发送文字到远程电脑</label>
        <input id="remote-text-input" data-controller-text-input type="text" maxlength="256" autocomplete="off" placeholder="可输入或粘贴中文、符号和短文本" required>
        <button class="toolbar-button" type="submit">${icon("send-horizontal")}发送文字</button>
        <button class="toolbar-button" type="button" data-controller-text-cancel>取消</button>
      </form>
      <div class="remote-viewport" data-remote-viewport tabindex="0" aria-label="远程 Windows 桌面，点击后可发送键盘和鼠标输入。">
        ${videoFailed
          ? '<div class="remote-waiting remote-waiting--error"><strong>远程画面暂时无法解码</strong><p>请更新 WebView2，或点击“刷新画面”再试一次。</p></div>'
          : config
            ? `<canvas class="remote-canvas" data-remote-canvas width="${config.width}" height="${config.height}"></canvas><div class="remote-video-starting" data-remote-video-starting>${icon("loader-circle", "controller-spinner")}<strong>正在启动远程画面</strong><p>正在接收并解码第一个加密视频帧。</p></div><span class="remote-cursor" data-remote-cursor aria-hidden="true" hidden>${icon("mouse-pointer-2")}</span>`
            : `<div class="remote-waiting">${icon("loader-circle", "controller-spinner")}<strong>正在准备远程画面</strong><p>DeskLink 协商视频流时，请保持此窗口打开。</p></div>`}
        <div class="remote-focus-hint">点击画面开始控制 · Ctrl+Alt+Delete 必须在主机本地操作</div>
      </div>
    </section>
  `;
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
        handleVideo(payload);
      }
    },
    (error) => {
      if (generation === channelGeneration) {
        handleVideoDeliveryError(error);
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
  const generation = ++channelGeneration;
  busy = false;
  cancelling = true;
  prepareControllerRender();
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
        };
      }
      if (signal.runtime.state !== "connected") {
        videoConfig = null;
        failedVideoConfig = null;
        resetVideoTelemetry();
        remoteDisplays = [];
        activeRemoteDisplayId = null;
        pendingRemoteDisplayId = null;
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
      remoteDisplays = signal.displays;
      activeRemoteDisplayId = signal.activeDisplayId;
      pendingRemoteDisplayId = null;
      const picker = document.querySelector<HTMLSelectElement>("[data-controller-display]");
      if (picker) {
        picker.value = String(activeRemoteDisplayId);
        picker.disabled = false;
      }
      break;
    }
    case "metrics": {
      relayCompletedFrames = signal.completedFrames;
      const element = document.querySelector<HTMLElement>("[data-controller-metrics]");
      if (element && videoConfig) {
        const total = signal.receivedVideoPackets + signal.droppedVideoPackets;
        const loss = total === 0 ? 0 : (signal.droppedVideoPackets / total) * 100;
        element.textContent = `${videoConfig.width} × ${videoConfig.height} · 中继 ${signal.completedFrames} 帧 · 前端 ${receivedVideoFrames} 帧 · 显示 ${decodedFrames} 帧 · 丢包率 ${loss.toFixed(1)}%`;
      }
      updateVideoStartingMessage();
      reportRenderMetrics();
      break;
    }
  }
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
  const context = canvas.getContext("2d", { alpha: false });
  if (!context) {
    failedVideoConfig = configKey;
    feedback = { tone: "error", message: "DeskLink 无法创建远程桌面绘制区域。" };
    queueMicrotask(requestRender);
    return;
  }
  setupRemoteGeometry(viewport, canvas);
  startVideoDecoder(canvas, context, configKey, "hardware");
  bindRemoteInput(viewport, canvas);
  viewport.focus({ preventScroll: true });
}

function handleVideo(payload: VideoPayload): void {
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
  if (!keyframe && decoder.decodeQueueSize > 4) {
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
              context.drawImage(nextFrame, 0, 0, canvas.width, canvas.height);
              if (decodedFrames === 0 && videoConfigReceivedAtMs !== null) {
                firstFrameMs = Math.max(0, Date.now() - videoConfigReceivedAtMs);
              }
              decodedFrames += 1;
              consecutiveDecoderStalls = 0;
              const waiting = document.querySelector<HTMLElement>("[data-remote-video-starting]");
              if (waiting) {
                waiting.hidden = true;
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
    const context = canvas?.getContext("2d", { alpha: false });
    if (canvas && context) {
      decoderPreference = "software";
      queueMicrotask(() => startVideoDecoder(canvas, context, configKey, "software"));
      return;
    }
  }
  if (consecutiveDecoderStalls <= 2) {
    const canvas = document.querySelector<HTMLCanvasElement>("[data-remote-canvas]");
    const context = canvas?.getContext("2d", { alpha: false });
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
  if (
    pendingRemoteDisplayId !== null
    || displayId === activeRemoteDisplayId
    || !remoteDisplays.some((display) => display.id === displayId)
  ) {
    return;
  }
  pendingRemoteDisplayId = displayId;
  const picker = document.querySelector<HTMLSelectElement>("[data-controller-display]");
  if (picker) {
    picker.disabled = true;
    picker.value = String(displayId);
  }
  try {
    await selectControllerDisplay(displayId);
  } catch (error) {
    pendingRemoteDisplayId = null;
    if (picker?.isConnected) {
      picker.disabled = false;
      picker.value = String(activeRemoteDisplayId ?? "");
      picker.setCustomValidity(normalizeError(error));
      picker.reportValidity();
      picker.setCustomValidity("");
    }
  }
}

const pressedKeys = new Map<string, ControllerInput>();
const pressedButtons = new Set<"left" | "right" | "middle">();

function bindRemoteInput(viewport: HTMLElement, canvas: HTMLCanvasElement): void {
  let pendingPoint: { x: number; y: number } | null = null;
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
  viewport.addEventListener("pointermove", (event) => {
    if (!pointerInsideViewport) {
      pointerInsideViewport = true;
      const cursor = document.querySelector<HTMLElement>("[data-remote-cursor]");
      if (cursor) {
        cursor.hidden = true;
      }
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
    const button = mouseButton(event.button);
    if (!button) {
      return;
    }
    remoteCanvasBounds = canvas.getBoundingClientRect();
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
    sendPendingPoint();
    releaseInputState();
  };
  viewport.addEventListener("pointercancel", releaseCapturedInput);
  viewport.addEventListener("lostpointercapture", releaseCapturedInput);
  viewport.addEventListener("contextmenu", (event) => event.preventDefault());
  viewport.addEventListener("wheel", (event) => {
    event.preventDefault();
    sendPendingPoint();
    const deltaX = clampWheel(Math.round(event.deltaX));
    const deltaY = clampWheel(-Math.round(event.deltaY));
    if (deltaX !== 0 || deltaY !== 0) {
      fireInput({ kind: "wheel", deltaX, deltaY });
    }
  }, { passive: false });
  viewport.addEventListener("keydown", (event) => sendKeyboardEvent(event, true));
  viewport.addEventListener("keyup", (event) => sendKeyboardEvent(event, false));
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
  if (pressed) {
    const input: ControllerInput = {
      kind: "key",
      key: key.key,
      character: key.character,
      pressed: true,
      modifiers: keyboardModifiers(event),
    };
    pressedKeys.set(event.code, input);
    fireInput(input);
  } else {
    const prior = pressedKeys.get(event.code);
    if (!prior) {
      return;
    }
    pressedKeys.delete(event.code);
    fireInput({ ...prior, pressed: false, modifiers: keyboardModifiers(event) });
  }
}

function releaseInputState(): void {
  for (const input of pressedKeys.values()) {
    fireInput({ ...input, pressed: false, modifiers: 0 });
  }
  pressedKeys.clear();
  for (const button of pressedButtons) {
    fireInput({ kind: "mouseButton", button, pressed: false });
  }
  pressedButtons.clear();
}

function fireInput(input: ControllerInput): void {
  inputDispatcher.enqueue(input);
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
  viewport?.focus({ preventScroll: true });
}

function pointerPosition(event: PointerEvent, canvas: HTMLCanvasElement): { x: number; y: number } | null {
  const bounds = remoteCanvasBounds ?? canvas.getBoundingClientRect();
  return normalizedPointerPosition(event.clientX, event.clientY, bounds);
}

function updateRemoteCursor(x: number, y: number, visible: boolean): void {
  const cursor = document.querySelector<HTMLElement>("[data-remote-cursor]");
  const canvas = document.querySelector<HTMLCanvasElement>("[data-remote-canvas]");
  const viewport = document.querySelector<HTMLElement>("[data-remote-viewport]");
  if (!cursor || !canvas || !viewport) {
    return;
  }
  if (pointerInsideViewport) {
    cursor.hidden = true;
    return;
  }
  const canvasBounds = remoteCanvasBounds ?? canvas.getBoundingClientRect();
  const viewportBounds = remoteViewportBounds ?? viewport.getBoundingClientRect();
  const left = canvasBounds.left - viewportBounds.left
    + (x / MAX_POINTER_COORDINATE) * canvasBounds.width;
  const top = canvasBounds.top - viewportBounds.top
    + (y / MAX_POINTER_COORDINATE) * canvasBounds.height;
  cursor.style.transform = `translate3d(${left - 1}px, ${top - 1}px, 0)`;
  cursor.hidden = !visible;
}

function setupRemoteGeometry(viewport: HTMLElement, canvas: HTMLCanvasElement): void {
  const refresh = () => {
    remoteCanvasBounds = canvas.getBoundingClientRect();
    remoteViewportBounds = viewport.getBoundingClientRect();
  };
  refresh();
  remoteResizeObserver?.disconnect();
  remoteResizeObserver = new ResizeObserver(refresh);
  remoteResizeObserver.observe(viewport);
  remoteResizeObserver.observe(canvas);
}

async function toggleFullscreen(): Promise<void> {
  const viewport = document.querySelector<HTMLElement>("[data-remote-viewport]");
  if (!viewport) {
    return;
  }
  if (document.fullscreenElement) {
    await document.exitFullscreen();
  } else {
    await viewport.requestFullscreen();
    viewport.focus({ preventScroll: true });
  }
}

function toUint8Array(payload: VideoPayload): Uint8Array {
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
