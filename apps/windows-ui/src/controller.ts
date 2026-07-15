import {
  connectController,
  createControllerChannels,
  disconnectController,
  forgetController,
  getControllerSnapshot,
  probeRelay,
  reconnectController,
  requestControllerKeyframe,
  sendControllerInput,
} from "./api";
import type { ControllerChannels } from "./api";
import { parsePairingCode } from "./pairing-code";
import type {
  ControllerInput,
  ControllerSignal,
  ControllerSnapshot,
  ControllerVideoConfigSignal,
} from "./types";

type RenderRequest = () => void;
type ControllerFeedback = { tone: "success" | "error" | "info"; message: string } | null;
type VideoPayload = ArrayBuffer | Uint8Array | number[];

const FRAME_PREFIX_BYTES = 17;
const MAX_POINTER_COORDINATE = 1_000_000;
const MAX_WHEEL_DELTA = 1_200;

let snapshot: ControllerSnapshot | null = null;
let loading = true;
let busy = false;
let checkingRelay = false;
let feedback: ControllerFeedback = null;
let invitationDraft = "";
let relayDraft = "127.0.0.1:4433";
let serverNameDraft = "localhost";
let videoConfig: ControllerVideoConfigSignal | null = null;
let activeChannels: ControllerChannels | null = null;
let channelGeneration = 0;
let decoder: VideoDecoder | null = null;
let pointerFrame: number | null = null;
let requestRender: RenderRequest = () => {};
let decodedFrames = 0;

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
          <h1>控制另一台电脑</h1>
          <p>粘贴另一台电脑生成的连接码，然后回到那台电脑确认身份。</p>
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
    .querySelector<HTMLFormElement>("[data-controller-form]")
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
    void requestControllerKeyframe().catch(showOperationError);
  });
  document.querySelector<HTMLButtonElement>("[data-controller-fullscreen]")?.addEventListener("click", () => {
    void toggleFullscreen();
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
    busy || checkingRelay || ["connecting", "waitingApproval", "reconnecting"].includes(snapshot?.runtime.state ?? "");
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
          <div><h2>首次连接这台电脑</h2><p>在另一台电脑的“已批准设备”页面创建连接码，然后完整粘贴到这里。</p></div>
        </div>
        <form class="controller-form" data-controller-form>
          <label class="field">
            <span>连接码</span>
            <textarea name="invitation" data-controller-invitation rows="6" maxlength="1024" placeholder="在这里粘贴另一台电脑生成的完整连接码" required autocomplete="off" spellcheck="false" ${isWorking ? "disabled" : ""}>${escapeHtml(invitationDraft)}</textarea>
          </label>
          <div class="connection-code-status connection-code-status--${codeStatus.tone}" data-controller-code-status role="status">
            <span aria-hidden="true"></span><p>${escapeHtml(codeStatus.text)}</p>
          </div>
          <details class="controller-network-details" ${invitationDraft.trim() && !recognizedCode ? "open" : ""}>
            <summary>检查或手动填写连接地址</summary>
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
            <button class="button button--primary" type="submit" ${isWorking ? "disabled" : ""} ${isWorking ? 'aria-busy="true"' : ""}>
              ${checkingRelay ? "等待检测完成" : isWorking ? '<span class="button-spinner" aria-hidden="true"></span> 正在连接' : "连接并请求批准"}
            </button>
            <button class="button button--secondary" type="button" data-controller-probe ${isWorking ? "disabled" : ""}>
              ${checkingRelay ? "正在检测…" : "先检测网络"}
            </button>
            <span>发起连接后，请回到另一台电脑核对并批准本机身份。</span>
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
    <div class="controller-security-note">
      <span class="security-note-mark" aria-hidden="true"></span>
      <div><strong>两台电脑分别保留自己的身份</strong><p>中继服务器只转发加密流量。新的控制端必须在主机上获得准确的本地批准后，才能查看画面或发送输入。</p></div>
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
  return `
    <section class="remote-session" aria-label="当前远程控制会话">
      <div class="remote-toolbar">
        <div class="remote-toolbar-status">
          <span class="remote-live-dot" aria-hidden="true"></span>
          <div><strong>实时远程桌面</strong><small data-controller-metrics>${config ? `${config.width} × ${config.height} · 已加密` : "正在等待首个视频画面"}</small></div>
        </div>
        <div class="remote-toolbar-actions">
          <button class="toolbar-button" type="button" data-controller-keyframe title="刷新远程画面">刷新画面</button>
          <button class="toolbar-button" type="button" data-controller-fullscreen>全屏</button>
          <button class="toolbar-button toolbar-button--danger" type="button" data-controller-disconnect>断开连接</button>
        </div>
      </div>
      <div class="remote-viewport" data-remote-viewport tabindex="0" aria-label="远程 Windows 桌面，点击后可发送键盘和鼠标输入。">
        ${config ? `<canvas class="remote-canvas" data-remote-canvas width="${config.width}" height="${config.height}"></canvas><span class="remote-cursor" data-remote-cursor aria-hidden="true" hidden></span>` : '<div class="remote-waiting"><span class="controller-spinner" aria-hidden="true"></span><strong>正在准备远程画面</strong><p>DeskLink 协商视频流时，请保持此窗口打开。</p></div>'}
        <div class="remote-focus-hint">点击画面开始控制 · Ctrl+Alt+Delete 必须在主机本地操作</div>
      </div>
    </section>
  `;
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
  const form = document.querySelector<HTMLFormElement>("[data-controller-form]");
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
  decoder = new VideoDecoder({
    output: (frame) => {
      context.drawImage(frame, 0, 0, canvas.width, canvas.height);
      frame.close();
      decodedFrames += 1;
    },
    error: () => {
      feedback = { tone: "error", message: "远程视频解码器已停止，请刷新画面或重新连接主机。" };
      decoder = null;
      requestRender();
    },
  });
  decoder.configure({
    codec: codecFromSequenceHeader(new Uint8Array(videoConfig.sequenceHeader)),
    codedWidth: videoConfig.width,
    codedHeight: videoConfig.height,
    hardwareAcceleration: "prefer-hardware",
    optimizeForLatency: true,
  });
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

function keyboardKey(value: string): { key: string; character?: string } | null {
  const named: Record<string, string> = {
    Enter: "enter",
    Escape: "escape",
    Backspace: "backspace",
    Tab: "tab",
    ArrowUp: "arrowUp",
    ArrowDown: "arrowDown",
    ArrowLeft: "arrowLeft",
    ArrowRight: "arrowRight",
  };
  if (named[value]) {
    return { key: named[value] };
  }
  return Array.from(value).length === 1 ? { key: "character", character: value } : null;
}

function keyboardModifiers(event: KeyboardEvent): number {
  return Number(event.shiftKey) | (Number(event.ctrlKey) << 1) | (Number(event.altKey) << 2) | (Number(event.metaKey) << 3);
}

function mouseButton(button: number): "left" | "right" | "middle" | null {
  return button === 0 ? "left" : button === 1 ? "middle" : button === 2 ? "right" : null;
}

function clampWheel(value: number): number {
  return Math.max(-MAX_WHEEL_DELTA, Math.min(MAX_WHEEL_DELTA, value));
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

function codecFromSequenceHeader(header: Uint8Array): string {
  for (let index = 0; index < header.length - 7; index += 1) {
    const fourByteStart = header[index] === 0 && header[index + 1] === 0 && header[index + 2] === 0 && header[index + 3] === 1;
    const threeByteStart = header[index] === 0 && header[index + 1] === 0 && header[index + 2] === 1;
    const nalIndex = index + (fourByteStart ? 4 : threeByteStart ? 3 : 0);
    if (nalIndex !== index && (header[nalIndex]! & 0x1f) === 7 && nalIndex + 3 < header.length) {
      return `avc1.${hexByte(header[nalIndex + 1]!)}${hexByte(header[nalIndex + 2]!)}${hexByte(header[nalIndex + 3]!)}`;
    }
  }
  return "avc1.42E01E";
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

function hexByte(value: number): string {
  return value.toString(16).padStart(2, "0").toUpperCase();
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

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}
