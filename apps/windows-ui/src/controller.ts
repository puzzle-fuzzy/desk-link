import {
  connectDevice,
  connectController,
  createControllerChannels,
  disconnectController,
  forgetController,
  getControllerSnapshot,
  probeRelay,
  reconnectController,
  requestControllerKeyframe,
  sendControllerInput,
  sendControllerText,
} from "./api";
import type { ControllerChannels } from "./api";
import {
  deviceCredentialsAreValid,
  formatDeviceId,
  normalizeTemporaryPassword,
} from "./device-credentials";
import { parsePairingCode } from "./pairing-code";
import { MANAGED_RELAY_ADDRESS, MANAGED_RELAY_SERVER_NAME } from "./product-config";
import { escapeHtml } from "./html";
import {
  MAX_POINTER_COORDINATE,
  clampWheel,
  keyboardKey,
  keyboardModifiers,
  mouseButton,
} from "./remote-input";
import type {
  ControllerInput,
  ControllerSignal,
  ControllerSnapshot,
  ControllerVideoConfigSignal,
} from "./types";
import { h264CodecFromSequenceHeader, videoConfigKey } from "./video-config";

type RenderRequest = () => void;
type ControllerFeedback = { tone: "success" | "error" | "info"; message: string } | null;
type VideoPayload = ArrayBuffer | Uint8Array | number[];

const FRAME_PREFIX_BYTES = 17;

let snapshot: ControllerSnapshot | null = null;
let loading = true;
let busy = false;
let checkingRelay = false;
let feedback: ControllerFeedback = null;
let deviceIdDraft = "";
let temporaryPasswordDraft = "";
let invitationDraft = "";
let relayDraft = MANAGED_RELAY_ADDRESS;
let serverNameDraft = MANAGED_RELAY_SERVER_NAME;
let videoConfig: ControllerVideoConfigSignal | null = null;
let activeChannels: ControllerChannels | null = null;
let channelGeneration = 0;
let decoder: VideoDecoder | null = null;
let pointerFrame: number | null = null;
let requestRender: RenderRequest = () => {};
let decodedFrames = 0;
let textSending = false;
let failedVideoConfig: string | null = null;

export async function initializeController(renderer: RenderRequest): Promise<void> {
  requestRender = renderer;
  try {
    snapshot = await getControllerSnapshot();
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    loading = false;
    requestRender();
  }
}

export function prepareControllerRender(): void {
  releaseInputState();
  if (pointerFrame !== null) {
    window.cancelAnimationFrame(pointerFrame);
    pointerFrame = null;
  }
  if (decoder && decoder.state !== "closed") {
    decoder.close();
  }
  decoder = null;
}

export function renderControllerView(): string {
  if (loading) {
    return `
      <div class="controller-loading" aria-live="polite">
        <span class="controller-spinner" aria-hidden="true"></span>
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
          <h1>连接另一台电脑</h1>
          <p>输入主机上显示的设备 ID 和临时密码，然后在主机上确认连接。</p>
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
  document
    .querySelector<HTMLFormElement>("[data-controller-legacy-form]")
    ?.addEventListener("submit", (event) => void submitInvitation(event));
  document.querySelector<HTMLTextAreaElement>("[data-controller-invitation]")?.addEventListener("input", (event) => {
    updatePairingCodePreview((event.currentTarget as HTMLTextAreaElement).value);
  });
  document.querySelector<HTMLInputElement>("[data-controller-relay-address]")?.addEventListener("input", (event) => {
    relayDraft = (event.currentTarget as HTMLInputElement).value;
  });
  document.querySelector<HTMLInputElement>("[data-controller-server-name]")?.addEventListener("input", (event) => {
    serverNameDraft = (event.currentTarget as HTMLInputElement).value;
  });
  document.querySelector<HTMLButtonElement>("[data-controller-probe]")?.addEventListener("click", () => {
    void checkRelayConnection();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-reconnect]")?.addEventListener("click", () => {
    void beginSavedConnection();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-forget]")?.addEventListener("click", () => {
    void forgetSavedConnection();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-disconnect]")?.addEventListener("click", () => {
    void endConnection();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-keyframe]")?.addEventListener("click", () => {
    retryVideo();
  });
  document.querySelector<HTMLButtonElement>("[data-controller-fullscreen]")?.addEventListener("click", () => {
    void toggleFullscreen();
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

function updatePairingCodePreview(value: string): void {
  invitationDraft = value;
  const relayInput = document.querySelector<HTMLInputElement>("[data-controller-relay-address]");
  const serverNameInput = document.querySelector<HTMLInputElement>("[data-controller-server-name]");
  const status = document.querySelector<HTMLElement>("[data-controller-code-status]");
  const parsed = parsePairingCode(value, relayInput?.value ?? relayDraft, serverNameInput?.value ?? serverNameDraft);
  if (parsed) {
    relayDraft = parsed.relayAddress;
    serverNameDraft = parsed.serverName;
    if (relayInput) {
      relayInput.value = parsed.relayAddress;
    }
    if (serverNameInput) {
      serverNameInput.value = parsed.serverName;
    }
  }
  if (!status) {
    return;
  }
  const tone = !value.trim() ? "empty" : parsed ? "ready" : "attention";
  const text = !value.trim()
    ? "粘贴完整连接码后，DeskLink 会自动填写连接地址。"
    : parsed
      ? `已识别连接码，将连接 ${parsed.relayAddress}。`
      : "连接码尚不完整，请回到另一台电脑重新复制完整内容。";
  status.className = `connection-code-status connection-code-status--${tone}`;
  const message = status.querySelector("p");
  if (message) {
    message.textContent = text;
  }
}

function renderFeedback(item: NonNullable<ControllerFeedback>): string {
  return `
    <div class="feedback feedback--${item.tone}" role="${item.tone === "error" ? "alert" : "status"}" aria-live="${item.tone === "error" ? "assertive" : "polite"}">
      <span class="feedback-symbol" aria-hidden="true"></span>
      <span>${escapeHtml(item.message)}</span>
      <button type="button" class="feedback-close" data-controller-dismiss aria-label="关闭消息">×</button>
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
  const saved = snapshot?.savedConnection;
  const isWorking =
    busy
    || checkingRelay
    || ["finding", "connecting", "waitingApproval", "reconnecting"].includes(snapshot?.runtime.state ?? "");
  const credentialsReady = deviceCredentialsAreValid(deviceIdDraft, temporaryPasswordDraft);
  const recognizedCode = parsePairingCode(invitationDraft, relayDraft, serverNameDraft);
  const codeStatus = !invitationDraft.trim()
    ? { tone: "empty", text: "粘贴完整连接码后，DeskLink 会自动填写连接地址。" }
    : recognizedCode
      ? { tone: "ready", text: `已识别连接码，将连接 ${recognizedCode.relayAddress}。` }
      : { tone: "attention", text: "连接码尚不完整，请回到另一台电脑重新复制完整内容。" };
  return `
    <div class="controller-connect-grid">
      <section class="controller-card controller-card--primary">
        <div class="controller-card-heading">
          <div><h2>连接远程设备</h2><p>在另一台电脑打开 DeskLink，查看本机 ID 并生成临时密码。</p></div>
        </div>
        <form class="controller-form controller-device-form" data-controller-device-form>
          <label class="field device-credential-field">
            <span>设备 ID</span>
            <input
              class="device-id-input"
              name="deviceId"
              data-controller-device-id
              value="${escapeHtml(deviceIdDraft)}"
              inputmode="numeric"
              maxlength="15"
              placeholder="123 456 789 012"
              aria-describedby="controller-device-hint"
              required
              autocomplete="off"
              spellcheck="false"
              ${isWorking ? "disabled" : ""}
            >
          </label>
          <label class="field device-credential-field">
            <span>临时密码</span>
            <input
              class="temporary-password-input"
              name="temporaryPassword"
              data-controller-password
              value="${escapeHtml(temporaryPasswordDraft)}"
              maxlength="8"
              placeholder="8 位临时密码"
              aria-describedby="controller-device-hint"
              required
              autocomplete="one-time-code"
              autocapitalize="characters"
              spellcheck="false"
              ${isWorking ? "disabled" : ""}
            >
          </label>
          <p class="controller-device-hint" id="controller-device-hint">临时密码仅在主机当前连接窗口内有效。</p>
          <div class="controller-form-actions">
            <button class="button button--primary" type="submit" data-controller-device-submit ${isWorking || !credentialsReady ? "disabled" : ""} ${isWorking ? 'aria-busy="true"' : ""}>
              ${isWorking ? '<span class="button-spinner" aria-hidden="true"></span> 正在查找设备' : "查找并连接设备"}
            </button>
            <span>找到设备后，主机会显示本次控制请求。</span>
          </div>
        </form>
      </section>

      <aside class="controller-card controller-card--saved">
        <div class="controller-card-heading">
          <div><h2>重新连接已批准电脑</h2><p>首次批准后，可以直接从这里重新连接。</p></div>
        </div>
        ${saved ? renderSavedConnection(isWorking) : renderNoSavedConnection()}
      </aside>
    </div>
    <details class="controller-legacy-panel" ${invitationDraft.trim() && !recognizedCode ? "open" : ""}>
      <summary>使用旧版连接码</summary>
      <div class="controller-legacy-content">
        <p>仅用于连接尚未升级到设备 ID 的旧版 DeskLink。</p>
        <form class="controller-form" data-controller-legacy-form>
          <label class="field">
            <span>旧版连接码</span>
            <textarea name="invitation" data-controller-invitation rows="4" maxlength="1024" placeholder="粘贴完整连接码" required autocomplete="off" spellcheck="false" ${isWorking ? "disabled" : ""}>${escapeHtml(invitationDraft)}</textarea>
          </label>
          <div class="connection-code-status connection-code-status--${codeStatus.tone}" data-controller-code-status role="status">
            <span aria-hidden="true"></span><p>${escapeHtml(codeStatus.text)}</p>
          </div>
          <details class="controller-network-details">
            <summary>高级连接设置</summary>
            <div class="field-grid field-grid--controller">
              <label class="field">
                <span>中继地址</span>
                <input name="relayAddress" data-controller-relay-address value="${escapeHtml(recognizedCode?.relayAddress ?? relayDraft)}" placeholder="relay.example.com:4433" required autocomplete="off" spellcheck="false" ${isWorking ? "disabled" : ""}>
              </label>
              <label class="field">
                <span>TLS 服务器名称</span>
                <input name="serverName" data-controller-server-name value="${escapeHtml(recognizedCode?.serverName ?? serverNameDraft)}" placeholder="relay.example.com" required autocomplete="off" spellcheck="false" ${isWorking ? "disabled" : ""}>
              </label>
            </div>
          </details>
          <div class="controller-form-actions">
            <button class="button button--secondary" type="submit" ${isWorking ? "disabled" : ""}>使用连接码</button>
            <button class="button button--secondary" type="button" data-controller-probe ${isWorking ? "disabled" : ""}>${checkingRelay ? "正在检测…" : "检测中继网络"}</button>
          </div>
        </form>
      </div>
    </details>
    <div class="controller-security-note">
      <span class="security-note-mark" aria-hidden="true"></span>
      <div><strong>连接仍需主机确认</strong><p>设备 ID 只用于查找在线主机，远程画面和输入经过端到端加密；新控制端必须在主机上获得本地批准。</p></div>
    </div>
  `;
}

function renderSavedConnection(isWorking: boolean): string {
  const saved = snapshot?.savedConnection;
  if (!saved) {
    return "";
  }
  return `
    <div class="saved-controller">
      <div class="saved-controller-mark" aria-hidden="true"><span></span></div>
      <div class="saved-controller-copy">
        <strong>${escapeHtml(saved.relayAddress)}</strong>
        <span>${escapeHtml(saved.serverName)}</span>
        <code title="${escapeHtml(saved.hostDeviceId)}">主机 ${escapeHtml(compact(saved.hostDeviceId))}</code>
      </div>
    </div>
    ${snapshot?.connectionError ? `<p class="inline-error">${escapeHtml(snapshot.connectionError)}</p>` : ""}
    <div class="saved-controller-actions">
      <button class="button button--primary" type="button" data-controller-reconnect ${isWorking ? "disabled" : ""}>重新连接</button>
      <button class="button button--secondary" type="button" data-controller-forget ${isWorking ? "disabled" : ""}>移除记录</button>
    </div>
  `;
}

function renderNoSavedConnection(): string {
  return `
    <div class="controller-empty">
      <span aria-hidden="true"></span>
      <strong>还没有已批准的电脑</strong>
      <p>首次批准连接后，可在这里一键重新连接。</p>
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
          <button class="toolbar-button" type="button" data-controller-text title="发送中文、符号或一段文字">发送文字</button>
          <button class="toolbar-button" type="button" data-controller-keyframe title="刷新远程画面">刷新画面</button>
          <button class="toolbar-button" type="button" data-controller-fullscreen>全屏</button>
          <button class="toolbar-button toolbar-button--danger" type="button" data-controller-disconnect>断开连接</button>
        </div>
      </div>
      <form class="remote-text-entry" data-controller-text-form data-controller-text-panel hidden>
        <label for="remote-text-input">发送文字到远程电脑</label>
        <input id="remote-text-input" data-controller-text-input type="text" maxlength="256" autocomplete="off" placeholder="可输入或粘贴中文、符号和短文本" required>
        <button class="toolbar-button" type="submit">发送文字</button>
        <button class="toolbar-button" type="button" data-controller-text-cancel>取消</button>
      </form>
      <div class="remote-viewport" data-remote-viewport tabindex="0" aria-label="远程 Windows 桌面，点击后可发送键盘和鼠标输入。">
        ${videoFailed
          ? '<div class="remote-waiting remote-waiting--error"><strong>远程画面暂时无法解码</strong><p>请更新 WebView2，或点击“刷新画面”再试一次。</p></div>'
          : config
            ? `<canvas class="remote-canvas" data-remote-canvas width="${config.width}" height="${config.height}"></canvas><span class="remote-cursor" data-remote-cursor aria-hidden="true" hidden></span>`
            : '<div class="remote-waiting"><span class="controller-spinner" aria-hidden="true"></span><strong>正在准备远程画面</strong><p>DeskLink 协商视频流时，请保持此窗口打开。</p></div>'}
        <div class="remote-focus-hint">点击画面开始控制 · Ctrl+Alt+Delete 必须在主机本地操作</div>
      </div>
    </section>
  `;
}

function updateDeviceSubmitState(): void {
  const submit = document.querySelector<HTMLButtonElement>("[data-controller-device-submit]");
  if (submit) {
    submit.disabled = busy || !deviceCredentialsAreValid(deviceIdDraft, temporaryPasswordDraft);
  }
}

async function submitDevice(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  if (busy) {
    return;
  }
  const form = event.currentTarget as HTMLFormElement;
  const data = new FormData(form);
  deviceIdDraft = formatDeviceId(String(data.get("deviceId") ?? ""));
  temporaryPasswordDraft = normalizeTemporaryPassword(String(data.get("temporaryPassword") ?? ""));
  if (!deviceCredentialsAreValid(deviceIdDraft, temporaryPasswordDraft)) {
    feedback = { tone: "error", message: "请输入完整的 12 位设备 ID 和 8 位临时密码。" };
    requestRender();
    return;
  }
  const started = await beginConnection((channels) =>
    connectDevice(
      { deviceId: deviceIdDraft, temporaryPassword: temporaryPasswordDraft },
      channels,
    ),
  );
  if (started) {
    temporaryPasswordDraft = "";
  }
}

async function submitInvitation(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  if (busy) {
    return;
  }
  const form = event.currentTarget as HTMLFormElement;
  if (!form.reportValidity()) {
    return;
  }
  const data = new FormData(form);
  relayDraft = String(data.get("relayAddress") ?? "").trim();
  serverNameDraft = String(data.get("serverName") ?? "").trim();
  const pairing = parsePairingCode(String(data.get("invitation") ?? ""), relayDraft, serverNameDraft);
  if (!pairing) {
    feedback = { tone: "error", message: "配对连接码无效，请从主机重新复制完整内容。" };
    requestRender();
    return;
  }
  relayDraft = pairing.relayAddress;
  serverNameDraft = pairing.serverName;
  invitationDraft = pairing.invitation;
  const started = await beginConnection((channels) =>
    connectController(
      { relayAddress: relayDraft, serverName: serverNameDraft, invitation: invitationDraft },
      channels,
    ),
  );
  if (started) {
    invitationDraft = "";
  }
}

async function checkRelayConnection(): Promise<void> {
  if (busy || checkingRelay) {
    return;
  }
  const form = document.querySelector<HTMLFormElement>("[data-controller-legacy-form]");
  if (!form || !form.reportValidity()) {
    return;
  }
  const data = new FormData(form);
  relayDraft = String(data.get("relayAddress") ?? "").trim();
  serverNameDraft = String(data.get("serverName") ?? "").trim();
  const rawInvitation = String(data.get("invitation") ?? "");
  const pairing = parsePairingCode(rawInvitation, relayDraft, serverNameDraft);
  if (!pairing) {
    feedback = { tone: "error", message: "配对连接码无效，请从主机重新复制完整内容后再检测。" };
    requestRender();
    return;
  }
  relayDraft = pairing.relayAddress;
  serverNameDraft = pairing.serverName;
  invitationDraft = pairing.invitation;
  checkingRelay = true;
  feedback = { tone: "info", message: "正在检测中继连接，最长等待 5 秒。" };
  requestRender();
  try {
    const result = await probeRelay({ relayAddress: relayDraft, serverName: serverNameDraft });
    feedback = {
      tone: "success",
      message: `${result.title}，${result.detail}（${result.elapsedMs} 毫秒）`,
    };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    checkingRelay = false;
    requestRender();
  }
}

async function beginSavedConnection(): Promise<void> {
  await beginConnection((channels) => reconnectController(channels));
}

async function beginConnection(operation: (channels: ControllerChannels) => Promise<ControllerSnapshot>): Promise<boolean> {
  if (busy) {
    return false;
  }
  busy = true;
  feedback = null;
  videoConfig = null;
  failedVideoConfig = null;
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
  );
  activeChannels = channels;
  requestRender();
  let started = false;
  try {
    snapshot = await operation(channels);
    started = true;
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
    try {
      snapshot = await getControllerSnapshot();
    } catch {
      // Keep the last known state; the actionable connection error remains visible.
    }
  } finally {
    busy = false;
    requestRender();
  }
  return started;
}

async function endConnection(): Promise<void> {
  if (busy) {
    return;
  }
  busy = true;
  releaseInputState();
  prepareControllerRender();
  try {
    snapshot = await disconnectController();
    videoConfig = null;
    channelGeneration += 1;
    activeChannels = null;
    feedback = { tone: "info", message: "远程控制已结束，已批准的电脑仍可重新连接。" };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    busy = false;
    requestRender();
  }
}

async function forgetSavedConnection(): Promise<void> {
  if (busy) {
    return;
  }
  busy = true;
  prepareControllerRender();
  try {
    snapshot = await forgetController();
    videoConfig = null;
    channelGeneration += 1;
    activeChannels = null;
    feedback = { tone: "success", message: "已从当前 Windows 账户中移除这台电脑的连接记录。" };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    busy = false;
    requestRender();
  }
}

function handleSignal(signal: ControllerSignal): void {
  switch (signal.kind) {
    case "status":
      if (snapshot) {
        snapshot.runtime = signal.runtime;
      } else {
        snapshot = { runtime: signal.runtime, savedConnection: null, connectionError: null };
      }
      if (signal.runtime.state !== "connected") {
        videoConfig = null;
        failedVideoConfig = null;
      }
      requestRender();
      break;
    case "videoConfig":
      videoConfig = signal;
      decodedFrames = 0;
      requestRender();
      break;
    case "cursor":
      updateRemoteCursor(signal.xMillionths, signal.yMillionths, signal.visible);
      break;
    case "metrics": {
      const element = document.querySelector<HTMLElement>("[data-controller-metrics]");
      if (element && videoConfig) {
        const total = signal.receivedVideoPackets + signal.droppedVideoPackets;
        const loss = total === 0 ? 0 : (signal.droppedVideoPackets / total) * 100;
        element.textContent = `${videoConfig.width} × ${videoConfig.height} · ${decodedFrames} 帧 · 丢包率 ${loss.toFixed(1)}%`;
      }
      break;
    }
  }
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
    feedback = { tone: "error", message: "当前 Windows WebView2 无法解码远程 H.264 画面。请更新 Microsoft Edge WebView2 Runtime 后重新打开 DeskLink。" };
    queueMicrotask(requestRender);
    return;
  }
  const context = canvas.getContext("2d", { alpha: false, desynchronized: true });
  if (!context) {
    feedback = { tone: "error", message: "DeskLink 无法创建远程桌面绘制区域。" };
    queueMicrotask(requestRender);
    return;
  }
  let nextDecoder: VideoDecoder;
  try {
    nextDecoder = new VideoDecoder({
      output: (frame) => {
        context.drawImage(frame, 0, 0, canvas.width, canvas.height);
        frame.close();
        decodedFrames += 1;
      },
      error: () => {
        if (decoder !== nextDecoder) {
          return;
        }
        failedVideoConfig = configKey;
        feedback = { tone: "error", message: "远程视频解码器已停止，请刷新画面或重新连接主机。" };
        decoder = null;
        requestRender();
      },
    });
    decoder = nextDecoder;
    nextDecoder.configure({
      codec: h264CodecFromSequenceHeader(new Uint8Array(config.sequenceHeader)),
      codedWidth: config.width,
      codedHeight: config.height,
      hardwareAcceleration: "prefer-hardware",
      optimizeForLatency: true,
    });
  } catch {
    failedVideoConfig = configKey;
    if (decoder && decoder.state !== "closed") {
      decoder.close();
    }
    decoder = null;
    feedback = {
      tone: "error",
      message: "当前 WebView2 不支持主机发送的 H.264 画面。请更新 Microsoft Edge WebView2 Runtime 后重试。",
    };
    queueMicrotask(requestRender);
    return;
  }
  bindRemoteInput(viewport, canvas);
  viewport.focus({ preventScroll: true });
  void requestControllerKeyframe().catch(showOperationError);
}

function handleVideo(payload: VideoPayload): void {
  if (!activeChannels || !decoder || decoder.state !== "configured" || !videoConfig) {
    return;
  }
  const bytes = toUint8Array(payload);
  if (bytes.byteLength <= FRAME_PREFIX_BYTES) {
    return;
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const keyframe = bytes[0] === 1;
  if (!keyframe && decoder.decodeQueueSize > 8) {
    return;
  }
  const timestamp = Number(view.getBigUint64(1, true));
  const accessUnit = bytes.subarray(FRAME_PREFIX_BYTES);
  const data = keyframe
    ? concatenate(new Uint8Array(videoConfig.sequenceHeader), accessUnit)
    : accessUnit;
  try {
    decoder.decode(new EncodedVideoChunk({
      type: keyframe ? "key" : "delta",
      timestamp,
      data,
    }));
  } catch {
    void requestControllerKeyframe().catch(showOperationError);
  }
}

function retryVideo(): void {
  failedVideoConfig = null;
  feedback = null;
  requestRender();
}

const pressedKeys = new Map<string, ControllerInput>();
const pressedButtons = new Set<"left" | "right" | "middle">();

function bindRemoteInput(viewport: HTMLElement, canvas: HTMLCanvasElement): void {
  let pendingPoint: { x: number; y: number } | null = null;
  const sendPendingPoint = () => {
    pointerFrame = null;
    if (pendingPoint) {
      fireInput({ kind: "mouseMove", ...pendingPoint });
      pendingPoint = null;
    }
  };
  viewport.addEventListener("pointermove", (event) => {
    const point = pointerPosition(event, canvas);
    if (!point) {
      return;
    }
    pendingPoint = point;
    if (pointerFrame === null) {
      pointerFrame = window.requestAnimationFrame(sendPendingPoint);
    }
  });
  viewport.addEventListener("pointerdown", (event) => {
    const button = mouseButton(event.button);
    if (!button) {
      return;
    }
    const point = pointerPosition(event, canvas);
    if (!point) {
      return;
    }
    event.preventDefault();
    viewport.focus({ preventScroll: true });
    viewport.setPointerCapture(event.pointerId);
    fireInput({ kind: "mouseMove", ...point });
    pressedButtons.add(button);
    fireInput({ kind: "mouseButton", button, pressed: true });
  });
  viewport.addEventListener("pointerup", (event) => {
    const button = mouseButton(event.button);
    if (!button) {
      return;
    }
    event.preventDefault();
    pressedButtons.delete(button);
    fireInput({ kind: "mouseButton", button, pressed: false });
  });
  viewport.addEventListener("pointercancel", releaseInputState);
  viewport.addEventListener("lostpointercapture", releaseInputState);
  viewport.addEventListener("contextmenu", (event) => event.preventDefault());
  viewport.addEventListener("wheel", (event) => {
    event.preventDefault();
    const deltaX = clampWheel(Math.round(event.deltaX));
    const deltaY = clampWheel(-Math.round(event.deltaY));
    if (deltaX !== 0 || deltaY !== 0) {
      fireInput({ kind: "wheel", deltaX, deltaY });
    }
  }, { passive: false });
  viewport.addEventListener("keydown", (event) => sendKeyboardEvent(event, true));
  viewport.addEventListener("keyup", (event) => sendKeyboardEvent(event, false));
  viewport.addEventListener("blur", releaseInputState);
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
  void sendControllerInput(input).catch(() => {
    // A reconnect can briefly reject input; the status channel owns user-facing recovery.
  });
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
  const bounds = canvas.getBoundingClientRect();
  if (
    bounds.width === 0
    || bounds.height === 0
    || event.clientX < bounds.left
    || event.clientX > bounds.right
    || event.clientY < bounds.top
    || event.clientY > bounds.bottom
  ) {
    return null;
  }
  const x = Math.max(0, Math.min(1, (event.clientX - bounds.left) / bounds.width));
  const y = Math.max(0, Math.min(1, (event.clientY - bounds.top) / bounds.height));
  return {
    x: Math.round(x * MAX_POINTER_COORDINATE),
    y: Math.round(y * MAX_POINTER_COORDINATE),
  };
}

function updateRemoteCursor(x: number, y: number, visible: boolean): void {
  const cursor = document.querySelector<HTMLElement>("[data-remote-cursor]");
  const canvas = document.querySelector<HTMLCanvasElement>("[data-remote-canvas]");
  const viewport = document.querySelector<HTMLElement>("[data-remote-viewport]");
  if (!cursor || !canvas || !viewport) {
    return;
  }
  const canvasBounds = canvas.getBoundingClientRect();
  const viewportBounds = viewport.getBoundingClientRect();
  cursor.style.left = `${canvasBounds.left - viewportBounds.left + (x / MAX_POINTER_COORDINATE) * canvasBounds.width}px`;
  cursor.style.top = `${canvasBounds.top - viewportBounds.top + (y / MAX_POINTER_COORDINATE) * canvasBounds.height}px`;
  cursor.hidden = !visible;
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

function concatenate(prefix: Uint8Array, data: Uint8Array): Uint8Array {
  const output = new Uint8Array(prefix.byteLength + data.byteLength);
  output.set(prefix, 0);
  output.set(data, prefix.byteLength);
  return output;
}

function toUint8Array(payload: VideoPayload): Uint8Array {
  if (payload instanceof Uint8Array) {
    return payload;
  }
  if (payload instanceof ArrayBuffer) {
    return new Uint8Array(payload);
  }
  return new Uint8Array(payload);
}

function compact(value: string): string {
  return value.length <= 20 ? value : `${value.slice(0, 8)}…${value.slice(-8)}`;
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
