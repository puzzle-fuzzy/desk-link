import "./styles.css";

import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";

import {
  cancelPairingSession,
  checkWindowsRelease,
  disableFixedAccessPassword,
  exportDiagnosticReport,
  getFixedAccessPassword,
  getHostSnapshot,
  getWindowsPreferences,
  openGithubRepository,
  openWindowsReleases,
  quitDeskLink,
  regenerateFixedAccessPassword,
  respondHostApproval,
  restartHost,
  revokeTrustedController,
  saveConnectionSettings,
  setupManagedConnection,
  setDiagnosticsSharing,
  setLaunchAtLogin,
  startPairingSession,
  uploadDiagnosticsNow,
} from "./api";
import type {
  ConnectionSettingsInput,
  ConnectionSummary,
  DiagnosticExportResult,
  FixedAccessSummary,
  HostSnapshot,
  PairingSessionSummary,
  TrustedControllerSummary,
  WindowsPreferencesSummary,
} from "./types";
import {
  bindControllerInteractions,
  initializeController,
  prepareControllerRender,
  renderControllerView,
} from "./controller";
import {
  MANAGED_RELAY_ADDRESS,
  MANAGED_RELAY_SERVER_NAME,
  isManagedRelay,
} from "./product-config";
import {
  DESKTOP_NAV_ITEMS,
  navigationViewFor,
  nextTabIndex,
  type DeskLinkView,
} from "./navigation";
import { LatestRequest } from "./latest-request";
import { escapeHtml } from "./html";
import { icon, renderLucideIcons } from "./icons";
import { hostStatusSummary } from "./host-status";
import {
  evaluateWindowsRelease,
  type WindowsUpdateCheck,
} from "./windows-update";

type View = "controller" | "connection" | "devices" | "pairing" | "fixedAccess" | "settings" | "about";
type Feedback = { tone: "success" | "error" | "info"; message: string } | null;
type WindowsUpdateState =
  | { kind: "idle" }
  | { kind: "checking" }
  | { kind: "error" }
  | WindowsUpdateCheck;

const applicationRoot = document.querySelector<HTMLElement>("#app");
if (!applicationRoot) {
  throw new Error("未找到 DeskLink 应用界面根节点");
}
const app: HTMLElement = applicationRoot;
const applicationWindow = getCurrentWindow();

let snapshot: HostSnapshot | null = null;
let activeView: View = "controller";
let renderedView: View | null = null;
let loading = true;
let saving = false;
let managedSetupBusy = false;
let hostRestartBusy = false;
let approvalBusy = false;
let focusedApprovalId: number | null = null;
let expiredApprovalId: number | null = null;
let pairingBusy = false;
let pairingSession: PairingSessionSummary | null = null;
let fixedAccess: FixedAccessSummary | null = null;
let fixedAccessBusy = false;
let fixedAccessConfirmation: "regenerate" | "disable" | null = null;
let diagnosticExportBusy = false;
let lastDiagnosticExport: DiagnosticExportResult | null = null;
let revokingFingerprint: string | null = null;
let feedback: Feedback = null;
let connectionDraft: ConnectionSettingsInput | null = null;
let connectionAdvancedOpen = false;
let preferences: WindowsPreferencesSummary | null = null;
let preferencesLoading = false;
let launchAtLoginBusy = false;
let diagnosticsSharingBusy = false;
let diagnosticsUploadBusy = false;
let quitConfirming = false;
let applicationVersion = "";
let windowsUpdate: WindowsUpdateState = { kind: "idle" };
const snapshotRequests = new LatestRequest();

function render(): void {
  const previousWorkspace = document.querySelector<HTMLElement>(".workspace");
  const preservedScrollTop = renderedView === activeView ? (previousWorkspace?.scrollTop ?? 0) : 0;
  const animateSurface = renderedView !== activeView;
  prepareControllerRender();
  app.innerHTML = `
    <div class="app-shell">
      ${renderHeader()}
      ${renderNavigation()}
      <section class="workspace ${activeView === "controller" ? "workspace--controller" : ""}" aria-busy="${loading}" data-surface-transition="${animateSurface}">
        ${feedback ? renderFeedback(feedback) : ""}
        ${loading ? renderLoading() : renderCurrentView()}
      </section>
    </div>
    ${renderHostApproval()}
  `;
  renderLucideIcons(app);
  const currentWorkspace = document.querySelector<HTMLElement>(".workspace");
  if (currentWorkspace && preservedScrollTop > 0) {
    currentWorkspace.scrollTop = preservedScrollTop;
  }
  renderedView = activeView;
  bindInteractions();
  focusNewApproval();
}

function renderHostApproval(): string {
  const approval = snapshot?.pendingApproval;
  if (!approval) {
    return "";
  }
  return `
    <div class="approval-backdrop">
      <section class="approval-dialog" role="dialog" aria-modal="true" aria-labelledby="approval-title" aria-describedby="approval-description approval-warning">
        <div class="approval-heading">
          ${icon(approval.identityChanged ? "shield-alert" : "shield-user", "approval-icon")}
          <div>
            <span class="approval-eyebrow">${approval.identityChanged ? "控制端安全身份已变化" : "新的远程控制请求"}</span>
            <h2 id="approval-title">${approval.identityChanged ? "是否信任这台电脑的新身份？" : "是否允许这台电脑控制本机？"}</h2>
          </div>
        </div>
        <p id="approval-description">${approval.identityChanged ? "这台设备的安全密钥与上次不同，可能是因为重装或重置。请先确认这是你认识的设备；允许后将替换旧身份。" : "允许后，对方可以查看你的屏幕，并使用鼠标和键盘。只有确认这是你认识的设备时才允许。"}</p>
        <dl class="approval-identity">
          <div>
            <dt>控制端设备</dt>
            <dd>${escapeHtml(formatApprovalDeviceId(approval.deviceId))}</dd>
          </div>
          <div>
            <dt>安全指纹</dt>
            <dd class="approval-fingerprint">${escapeHtml(approval.fingerprint)}</dd>
          </div>
        </dl>
        <p class="approval-warning" id="approval-warning">${icon("triangle-alert", "approval-warning-icon")}<span class="approval-warning-copy">${approval.identityChanged ? "仅当你确认对方刚刚重装或重置过 DeskLink 时才替换旧身份。" : "如果你没有主动发起连接，请选择“拒绝”。"}<strong data-approval-countdown>${escapeHtml(formatApprovalRemaining(approval.expiresAtUnixS))}</strong></span></p>
        <div class="approval-actions">
          <button class="button button--secondary" type="button" data-reject-host-approval ${approvalBusy ? "disabled" : ""}>${approvalBusy ? "正在处理…" : "拒绝"}</button>
          <button class="button button--primary" type="button" data-allow-host-approval ${approvalBusy ? "disabled" : ""}>${approval.identityChanged ? "确认并替换旧身份" : "允许本次控制"}</button>
        </div>
      </section>
    </div>
  `;
}

function formatApprovalDeviceId(deviceId: string): string {
  return deviceId.match(/.{1,4}/g)?.join("-") ?? deviceId;
}

function focusNewApproval(): void {
  const requestId = snapshot?.pendingApproval?.requestId ?? null;
  if (requestId === null) {
    focusedApprovalId = null;
    expiredApprovalId = null;
    return;
  }
  if (focusedApprovalId === requestId) {
    return;
  }
  focusedApprovalId = requestId;
  expiredApprovalId = null;
  window.requestAnimationFrame(() => {
    document.querySelector<HTMLButtonElement>("[data-reject-host-approval]")?.focus();
  });
}

function handleApprovalKeyboard(event: KeyboardEvent): void {
  const dialog = document.querySelector<HTMLElement>(".approval-dialog");
  if (!dialog || approvalBusy) {
    return;
  }
  if (event.key === "Escape") {
    event.preventDefault();
    void answerHostApproval(false);
    return;
  }
  if (event.key !== "Tab") {
    return;
  }
  const controls = Array.from(
    dialog.querySelectorAll<HTMLElement>('button:not([disabled]), [href], input:not([disabled]), [tabindex]:not([tabindex="-1"])'),
  );
  if (controls.length === 0) {
    event.preventDefault();
    return;
  }
  const first = controls[0];
  const last = controls.at(-1);
  if (!first || !last) {
    return;
  }
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
}

function renderHeader(): string {
  return `
    <header class="titlebar" data-tauri-drag-region>
      <div class="product-lockup" aria-label="DeskLink Windows 远程桌面" data-tauri-drag-region>
        ${icon("monitor-check", "product-mark")}
        <strong data-tauri-drag-region>DeskLink</strong>
      </div>
      ${snapshot ? renderHostStatusChip(snapshot) : ""}
      <div class="titlebar-drag-space" data-tauri-drag-region></div>
      <div class="titlebar-end">
        <div class="window-controls" aria-label="窗口控制">
          <button type="button" data-window-minimize aria-label="最小化 DeskLink" title="最小化">${icon("minus")}</button>
          <button type="button" data-window-maximize aria-label="最大化或还原 DeskLink" title="最大化或还原">${icon("square")}</button>
          <button class="window-control-close" type="button" data-window-close aria-label="关闭到系统托盘" title="关闭到系统托盘">${icon("x")}</button>
        </div>
      </div>
    </header>
  `;
}

function renderHostStatusChip(state: HostSnapshot): string {
  const status = hostStatusSummary(state);
  return `<button class="host-status-chip host-status-chip--${status.tone}" type="button" data-open-overview aria-label="${escapeHtml(status.title)}，${escapeHtml(status.detail)}，打开设置 / 诊断">${escapeHtml(status.title)}</button>`;
}

function renderNavigation(): string {
  const activeNavigationView = navigationViewFor(activeView as DeskLinkView);
  return `
    <nav class="section-nav" aria-label="DeskLink 功能导航" role="tablist">
      ${DESKTOP_NAV_ITEMS
        .map(
          ({ id, label }) => `
            <button
              class="nav-item ${activeNavigationView === id ? "nav-item--active" : ""}"
              type="button"
              role="tab"
              data-view="${id}"
              aria-selected="${activeNavigationView === id}"
              ${activeNavigationView === id ? 'tabindex="0"' : 'tabindex="-1"'}
            >${label}</button>
          `,
        )
        .join("")}
      <span class="nav-spacer" aria-hidden="true"></span>
      <button
        class="nav-utility ${activeView === "about" ? "nav-utility--active" : ""}"
        type="button"
        role="tab"
        data-view="about"
        aria-selected="${activeView === "about"}"
        ${activeView === "about" ? 'tabindex="0"' : 'tabindex="-1"'}
      >${icon("circle-help")}关于</button>
      <button class="nav-utility" type="button" data-open-github title="在浏览器中打开 DeskLink GitHub 仓库">${icon("git-fork")}GitHub</button>
    </nav>
  `;
}

function renderFeedback(item: NonNullable<Feedback>): string {
  return `
    <div class="feedback feedback--${item.tone}" role="${item.tone === "error" ? "alert" : "status"}" aria-live="${item.tone === "error" ? "assertive" : "polite"}">
      ${icon(item.tone === "success" ? "circle-check" : item.tone === "error" ? "circle-alert" : "info", "feedback-symbol")}
      <span>${escapeHtml(item.message)}</span>
      <button type="button" class="feedback-close" data-dismiss-feedback aria-label="关闭消息">${icon("x")}</button>
    </div>
  `;
}

function renderLoading(): string {
  return `
    <div class="loading-layout" aria-label="正在读取受保护的 DeskLink 状态">
      <div class="skeleton skeleton--status"></div>
      <div class="skeleton-row">
        <div class="skeleton"></div>
        <div class="skeleton"></div>
        <div class="skeleton"></div>
      </div>
      <div class="skeleton skeleton--list"></div>
    </div>
  `;
}

function renderCurrentView(): string {
  if (!snapshot) {
    return renderFatalState();
  }
  switch (activeView) {
    case "controller":
      return renderControllerWorkspace(snapshot);
    case "connection":
      return renderConnection(snapshot);
    case "devices":
      return renderDevices(snapshot);
    case "pairing":
      return renderPairing(snapshot);
    case "fixedAccess":
      return renderFixedAccess(snapshot);
    case "settings":
      return renderSettings();
    case "about":
      return renderAbout();
  }
}

function renderAbout(): string {
  return `
    <div class="page-layout page-layout--about">
      <section class="about-hero" aria-labelledby="about-heading">
        ${icon("monitor-check", "about-mark")}
        <div>
          <h1 id="about-heading">DeskLink</h1>
          <p>在自己的 Windows 电脑之间建立端到端加密的远程桌面连接。</p>
        </div>
        <span class="about-version">版本 ${escapeHtml(applicationVersion || "正在读取")}</span>
      </section>
      <dl class="about-details">
        <div><dt>隐私</dt><dd>远程画面、鼠标和键盘输入只在两台设备之间解密，中继服务器只转发加密数据。</dd></div>
        <div><dt>本地保护</dt><dd>设备记录和访问密码由当前 Windows 账户加密保存。</dd></div>
        <div><dt>源代码</dt><dd><button class="text-button about-github-link" type="button" data-open-github>${icon("git-fork")}查看 puzzle-fuzzy/desk-link</button></dd></div>
      </dl>
      <p class="about-footnote">关闭窗口后 DeskLink 会继续在系统托盘运行。只有在“设置”中选择退出，才会停止本机服务。</p>
    </div>
  `;
}

function renderFatalState(): string {
  return `
    <div class="empty-state empty-state--error">
      ${icon("circle-alert", "empty-symbol")}
      <h1>无法读取 DeskLink 状态</h1>
      <p>当前界面无法读取此 Windows 账户的本地状态，主机设置没有被修改。</p>
      <button class="button button--primary" type="button" data-refresh>重新读取</button>
    </div>
  `;
}

function renderControllerWorkspace(state: HostSnapshot): string {
  return `
    <div class="control-workspace">
      ${renderStateWarnings(state)}
      <div class="control-workspace-main">${renderControllerView()}</div>
    </div>
  `;
}

function renderDiagnosticSummary(state: HostSnapshot): string {
  const failed = state.diagnosticChecks.filter((check) => check.status === "failed").length;
  const warning = state.diagnosticChecks.filter((check) => check.status === "warning").length;
  if (failed === 0 && warning === 0 && !lastDiagnosticExport) {
    return "";
  }
  const summary = failed > 0 ? `${failed} 项需要处理` : warning > 0 ? `${warning} 项需要注意` : "全部检查通过";
  const checks = state.diagnosticChecks
    .map(
      (check) => `
        <li class="diagnostic-check diagnostic-check--${check.status}">
          <span class="diagnostic-check-mark" aria-hidden="true"></span>
          <code>${escapeHtml(check.code)}</code>
          <div>
            <strong>${escapeHtml(check.title)}</strong>
            <p>${escapeHtml(check.detail)}</p>
          </div>
          <small>${diagnosticStatusLabel(check.status)}</small>
        </li>
      `,
    )
    .join("");
  return `
    <section class="diagnostic-summary diagnostic-disclosure" aria-labelledby="diagnostic-summary-heading">
      <div class="section-heading">
        <div>
          <h2 id="diagnostic-summary-heading">双机连接诊断</h2>
          <p>连接遇到问题时，可展开检查结果或导出报告。</p>
        </div>
        <div class="diagnostic-actions">
          <span class="diagnostic-overall">${summary}</span>
          <button class="button button--secondary button--compact" type="button" data-export-diagnostics ${diagnosticExportBusy ? "disabled" : ""}>
            ${diagnosticExportBusy ? "正在导出…" : "导出诊断报告"}
          </button>
        </div>
      </div>
      <details class="diagnostic-details" ${failed > 0 ? "open" : ""}>
        <summary>${failed > 0 ? "查看需要处理的检查" : `查看 ${state.diagnosticChecks.length} 项检查结果`}</summary>
        <ul class="diagnostic-check-list" aria-label="双机连接检查结果">${checks}</ul>
      </details>
      ${
        lastDiagnosticExport
          ? `
            <div class="diagnostic-export-result" role="status">
              <div>
                <strong>最近导出：${escapeHtml(lastDiagnosticExport.reportId)}</strong>
                <span>${escapeHtml(lastDiagnosticExport.fileName)}</span>
              </div>
              <code title="${escapeHtml(lastDiagnosticExport.filePath)}">${escapeHtml(lastDiagnosticExport.filePath)}</code>
            </div>
          `
          : '<p class="diagnostic-privacy">报告会自动清除会话 ID、中继密钥、公钥和完整设备身份，只保留排查所需的运行状态、中继端点和最近事件。</p>'
      }
    </section>
  `;
}

function diagnosticStatusLabel(status: string): string {
  switch (status) {
    case "passed":
      return "通过";
    case "failed":
      return "失败";
    case "notApplicable":
      return "不适用";
    default:
      return "注意";
  }
}

function renderStateWarnings(state: HostSnapshot): string {
  const warnings = [state.connectionError, state.trustedError].filter(
    (warning): warning is string => Boolean(warning),
  );
  if (warnings.length === 0) {
    return "";
  }
  return `
    <div class="warning-stack">
      ${warnings
        .map(
          (warning) => `
            <div class="inline-warning" role="alert">
              ${icon("triangle-alert")}
              <p>${escapeHtml(warning)}</p>
            </div>
          `,
        )
        .join("")}
    </div>
  `;
}

function renderConnection(state: HostSnapshot): string {
  if (!connectionDraft) {
    connectionDraft = connectionToInput(state.connection);
  }
  const fields = connectionDraft;
  return `
    <div class="page-layout page-layout--form">
      <header class="page-heading">
        <div>
          <h1>共享此设备</h1>
          <p>保存后，这台电脑才能生成临时密码并等待另一台电脑连接。</p>
        </div>
        <div class="page-actions">
          <span class="storage-note">由 Windows DPAPI 加密保护</span>
        </div>
      </header>

      ${state.connectionError ? renderStateWarnings(state) : ""}

      <div class="connection-guidance share-device-card">
        ${icon("globe-lock", "connection-guidance-mark")}
        <div>
          <strong>${state.connection && isManagedRelay(state.connection.relayAddress, state.connection.serverName) ? "正在使用 DeskLink 公网中继" : "默认使用 DeskLink 公网中继"}</strong>
          <p>公网中继可在不同网络之间连接。只有需要使用自己维护的中继基础设施时，才修改下面两项。</p>
        </div>
      </div>

      <form class="connection-form" data-connection-form novalidate>
        <div class="field">
          <label for="relay-address">中继地址</label>
          <input
            id="relay-address"
            name="relayAddress"
            type="text"
            value="${escapeHtml(fields.relayAddress)}"
            placeholder="192.0.2.10:4433"
            autocomplete="off"
            spellcheck="false"
            required
          />
          <small>默认 ${MANAGED_RELAY_ADDRESS}；自建中继必须填写控制端可以访问的 IP 与 UDP 端口。</small>
        </div>

        <div class="field">
          <label for="server-name">TLS 服务器名称</label>
          <input
            id="server-name"
            name="serverName"
            type="text"
            value="${escapeHtml(fields.serverName)}"
            placeholder="relay.example.com"
            autocomplete="off"
            spellcheck="false"
            required
          />
          <small>默认 ${MANAGED_RELAY_SERVER_NAME}；自建中继必须与证书名称一致。</small>
        </div>

        <details class="advanced-settings field--wide" data-connection-advanced ${connectionAdvancedOpen ? "open" : ""}>
          <summary>
            <span>高级连接设置</span>
            <small>会话标识、密钥和视频流编号</small>
          </summary>
          <div class="advanced-settings-grid">
            <div class="field field--wide">
              <label for="session-id">会话 ID</label>
              <input
                id="session-id"
                name="sessionId"
                type="text"
                value="${escapeHtml(fields.sessionId)}"
                placeholder="32 位十六进制字符"
                minlength="32"
                maxlength="32"
                pattern="[0-9a-fA-F]{32}"
                autocomplete="off"
                spellcheck="false"
                required
              />
              <small>用于识别这台电脑的私有中继会话。</small>
            </div>

            <div class="field field--wide">
              <label for="relay-key">中继密钥</label>
              <div class="secret-input">
                <input
                  id="relay-key"
                  name="relayKey"
                  type="password"
                  value="${escapeHtml(fields.relayKey)}"
                  placeholder="${state.connection?.hasSavedKey ? "留空可保留已保存的密钥" : "64 位十六进制字符"}"
                  minlength="${state.connection?.hasSavedKey ? "0" : "64"}"
                  maxlength="64"
                  pattern="[0-9a-fA-F]{64}"
                  autocomplete="new-password"
                  spellcheck="false"
                  ${state.connection?.hasSavedKey ? "" : "required"}
                />
                <button type="button" class="secret-toggle" data-toggle-secret aria-label="显示中继密钥">显示</button>
              </div>
              <small>${state.connection?.hasSavedKey ? "密钥已保存，留空即可继续使用。" : "已自动生成，只会加密保存在当前 Windows 账户。"}</small>
            </div>

            <div class="field field--compact">
              <label for="stream-id">视频流 ID</label>
              <input
                id="stream-id"
                name="streamId"
                type="number"
                value="${escapeHtml(fields.streamId)}"
                min="1"
                step="1"
                required
              />
              <small>通常保持为 1，安全重连时会自动递增。</small>
            </div>
            <div class="advanced-settings-action">
              <button class="button button--secondary button--compact" type="button" data-generate-connection ${saving ? "disabled" : ""}>重新生成连接凭据</button>
              <small>重新生成后，已经配对的电脑需要再次配对。</small>
            </div>
          </div>
        </details>

        <div class="form-actions field--wide">
          <button class="button button--primary" type="submit" ${saving ? "disabled" : ""} ${saving ? 'aria-busy="true"' : ""}>
            ${saving ? "正在保存共享设置…" : "保存共享设置"}
          </button>
          <button class="button button--secondary" type="button" data-cancel-connection ${saving ? "disabled" : ""}>
            返回设置 / 诊断
          </button>
        </div>
      </form>
    </div>
  `;
}

function renderDevices(state: HostSnapshot): string {
  return `
    <div class="page-layout">
      <header class="page-heading">
        <div>
          <h1>已批准设备</h1>
          <p>这些电脑已经在本机完成身份核对，可以再次连接。</p>
        </div>
        <div class="page-actions">
          ${renderPairingAction(state, "primary")}
          <button class="button button--secondary" type="button" data-refresh>刷新设备</button>
        </div>
      </header>

      ${state.trustedError ? renderStateWarnings(state) : ""}

      <section class="device-register" aria-label="已批准设备身份列表">
        ${
          state.trustedControllers.length === 0
            ? renderDeviceEmptyState()
            : state.trustedControllers.map(renderDevice).join("")
        }
      </section>

      <div class="security-note">
        ${icon("shield-check", "security-note-mark")}
        <div>
          <strong>批准操作仅在本机完成</strong>
          <p>新的控制端无法自行加入此列表，DeskLink 必须在这台 Windows 设备上获得确认。</p>
        </div>
      </div>
    </div>
  `;
}

function renderDeviceEmptyState(): string {
  return `
    <div class="empty-state empty-state--devices">
      ${icon("monitor", "empty-device")}
      <h2>还没有已批准的电脑</h2>
      <p>生成一份临时密码，在另一台电脑输入本机 ID 后回到这里确认身份。</p>
      ${snapshot ? renderPairingAction(snapshot, "primary") : ""}
    </div>
  `;
}

function renderDevice(device: TrustedControllerSummary): string {
  const revoking = revokingFingerprint === device.fingerprint;
  return `
    <article class="device-record">
      <div class="device-record-heading">
        ${icon("monitor", "device-avatar")}
        <div>
          <h2>已批准电脑 ${escapeHtml(compactIdentifier(device.deviceId))}</h2>
          <p>批准于 ${formatDate(device.approvedAtUnixS)}</p>
        </div>
        <div class="device-actions">
          <span class="trusted-badge">已批准</span>
          <button
            class="button button--danger-quiet button--compact"
            type="button"
            data-revoke-controller="${escapeHtml(device.fingerprint)}"
            ${revokingFingerprint ? "disabled" : ""}
          >${revoking ? "等待 Windows 确认…" : "撤销访问"}</button>
        </div>
      </div>
      <dl class="identity-grid">
        <div>
          <dt>设备 ID</dt>
          <dd><code>${escapeHtml(device.deviceId)}</code></dd>
        </div>
        <div>
          <dt>公钥</dt>
          <dd><code>${escapeHtml(device.verifyKey)}</code></dd>
        </div>
        <div>
          <dt>指纹</dt>
          <dd><code>${escapeHtml(device.fingerprint)}</code></dd>
        </div>
      </dl>
    </article>
  `;
}

function renderPairingAction(
  state: HostSnapshot,
  presentation: "primary" | "secondary" | "text",
): string {
  const active = state.pairingActive;
  const connectedPairing = active && state.runtime.state === "connected";
  const disabled =
    pairingBusy ||
    connectedPairing ||
    (!active && (!state.connection || Boolean(state.trustedError)));
  const className = presentation === "text" ? "text-button" : `button button--${presentation}`;
  const action = active ? "data-open-pairing" : "data-start-pairing";
  const label = connectedPairing
    ? "连接进行中"
    : active
      ? "查看临时密码"
      : pairingBusy
        ? "正在生成临时密码…"
        : "生成临时密码";
  const title = !state.connection
    ? 'title="请先启用本机远程连接，再生成临时密码"'
    : state.trustedError
      ? 'title="已批准设备存储可用后才能生成临时密码"'
      : "";
  return `<button class="${className}" type="button" ${action} ${disabled ? "disabled" : ""} ${title}>${label}</button>`;
}

function renderPairing(state: HostSnapshot): string {
  const session = pairingSession;
  const active = state.pairingActive;
  return `
    <div class="page-layout page-layout--pairing">
      <header class="page-heading page-heading--pairing">
        <div>
          <button class="back-button" type="button" data-open-connection aria-label="返回共享此设备">${icon("arrow-left")}共享此设备</button>
          <h1>允许另一台电脑连接</h1>
          <p>在另一台电脑输入下面的设备 ID 和临时密码。</p>
        </div>
        <span class="pairing-state ${active ? "pairing-state--active" : ""}">
          <span aria-hidden="true"></span>${active ? "临时密码有效" : "临时密码已失效"}
        </span>
      </header>

      ${
        session
          ? `
            <section class="pairing-card" aria-labelledby="pairing-credentials-heading">
              <div class="pairing-card-heading">
                <div>
                  <span class="eyebrow">本次连接凭据</span>
                  <h2 id="pairing-credentials-heading">输入到另一台电脑</h2>
                </div>
                <strong data-pairing-countdown>${formatPairingRemaining(session.expiresAtUnixS)}</strong>
              </div>
              <div class="pairing-credentials">
                <div class="pairing-credential">
                  <span>设备 ID</span>
                  <strong id="pairing-device-id">${escapeHtml(session.deviceId)}</strong>
                  <button class="button button--secondary button--compact" type="button" data-copy-device-id ${pairingBusy ? "disabled" : ""}>复制设备 ID</button>
                </div>
                <div class="pairing-credential pairing-credential--password">
                  <span>临时密码</span>
                  <strong id="pairing-temporary-password">${escapeHtml(session.temporaryPassword)}</strong>
                  <button class="button button--secondary button--compact" type="button" data-copy-temporary-password ${pairingBusy ? "disabled" : ""}>复制临时密码</button>
                </div>
              </div>
              <ol class="pairing-steps" aria-label="连接步骤">
                <li><span>1</span><p>在另一台电脑打开“连接设备”</p></li>
                <li><span>2</span><p>输入设备 ID 和临时密码</p></li>
                <li><span>3</span><p>回到本机确认连接请求</p></li>
              </ol>
              <div class="pairing-card-actions">
                <button class="button button--secondary" type="button" data-cancel-pairing ${pairingBusy ? "disabled" : ""}>${pairingBusy ? "正在结束…" : "结束本次连接窗口"}</button>
              </div>
              <p class="secret-note" id="pairing-secret-note">临时密码仅供本次连接使用，请勿发送到公开聊天或工单。</p>
            </section>
          `
          : `
            <section class="pairing-card pairing-card--unavailable">
              ${icon(active ? "loader-circle" : "circle-x", active ? "empty-symbol icon-spin" : "empty-symbol")}
              <h2>${active ? "本机正在等待连接" : "上次连接没有完成"}</h2>
              <p>${
                active
                  ? "为保护连接安全，临时密码只显示在生成它的窗口中。如需重新获取，请结束本次连接窗口后重新生成。"
                  : "临时密码已过期或连接已经结束。请生成一份新密码后重新尝试。"
              }</p>
              <div class="pairing-card-actions">
                ${active
                  ? `<button class="button button--secondary" type="button" data-cancel-pairing ${pairingBusy ? "disabled" : ""}>结束当前等待</button>`
                  : `<button class="button button--primary" type="button" data-start-pairing ${pairingBusy ? "disabled" : ""}>重新生成临时密码</button>`}
              </div>
            </section>
          `
      }

      <div class="security-note security-note--pairing">
        ${icon("shield-check", "security-note-mark")}
        <div>
          <strong>下一步需要回到这台电脑确认</strong>
          <p>设备 ID 和临时密码只能用于找到本机。Windows 会显示控制端身份，确认后才允许查看画面和控制输入。</p>
        </div>
      </div>
    </div>
  `;
}

function renderFixedAccess(state: HostSnapshot): string {
  const enabled = state.fixedPasswordEnabled;
  const canEnable = Boolean(state.connection) && !state.fixedPasswordError;
  return `
    <div class="page-layout page-layout--pairing">
      <header class="page-heading page-heading--pairing">
        <div>
          <button class="back-button" type="button" data-open-overview aria-label="返回设置 / 诊断">${icon("arrow-left")}设置 / 诊断</button>
          <h1>固定访问密码</h1>
          <p>适合经常从自己的另一台电脑连接本机，无需每次重新生成临时密码。</p>
        </div>
        <span class="pairing-state ${enabled ? "pairing-state--active" : ""}">
          <span aria-hidden="true"></span>${enabled ? "固定密码已启用" : "固定密码未启用"}
        </span>
      </header>

      ${state.fixedPasswordError
        ? `<section class="pairing-card pairing-card--unavailable">${icon("circle-alert", "empty-symbol")}<h2>无法读取固定密码</h2><p>${escapeHtml(state.fixedPasswordError)}</p></section>`
        : enabled
          ? fixedAccess
            ? `
              <section class="pairing-card" aria-labelledby="fixed-access-heading">
                <div class="pairing-card-heading">
                  <div><span class="eyebrow">长期访问凭据</span><h2 id="fixed-access-heading">输入到另一台电脑</h2></div>
                  <strong>由 Windows 加密保护</strong>
                </div>
                <div class="pairing-credentials">
                  <div class="pairing-credential">
                    <span>设备 ID</span>
                    <strong id="fixed-access-device-id">${escapeHtml(fixedAccess.deviceId)}</strong>
                    <button class="button button--secondary button--compact" type="button" data-copy-fixed-device-id>复制设备 ID</button>
                  </div>
                  <div class="pairing-credential pairing-credential--password">
                    <span>固定密码</span>
                    <strong id="fixed-access-password">${escapeHtml(fixedAccess.password)}</strong>
                    <button class="button button--secondary button--compact" type="button" data-copy-fixed-password>复制固定密码</button>
                  </div>
                </div>
                <div class="pairing-card-actions">
                  ${fixedAccessConfirmation
                    ? `<div class="fixed-access-confirmation" role="group" aria-label="确认${fixedAccessConfirmation === "regenerate" ? "更换" : "关闭"}固定密码">
                        <strong>${fixedAccessConfirmation === "regenerate" ? "更换后，其他电脑保存的旧密码会立即失效。" : "关闭后，其他电脑将无法再用固定密码查找本机。"}</strong>
                        <button class="button button--secondary" type="button" data-cancel-fixed-access-action>保留当前密码</button>
                        <button class="button button--danger-quiet" type="button" data-confirm-fixed-access-action>${fixedAccessConfirmation === "regenerate" ? "更换并使旧密码失效" : "关闭固定密码"}</button>
                      </div>`
                    : `<button class="button button--secondary" type="button" data-regenerate-fixed-access ${fixedAccessBusy ? "disabled" : ""}>${fixedAccessBusy ? "正在更换…" : "更换固定密码"}</button>
                       <button class="button button--danger-quiet" type="button" data-disable-fixed-access ${fixedAccessBusy ? "disabled" : ""}>关闭固定密码</button>`}
                </div>
                <p class="secret-note">更换或关闭后，旧固定密码会立即失效；已批准设备仍可使用保存的安全连接。</p>
              </section>
            `
            : `<section class="pairing-card pairing-card--unavailable">${icon("loader-circle", "controller-spinner")}<h2>正在读取固定密码</h2><p>密码只会在你主动打开此页面时解密显示。</p></section>`
          : `
            <section class="pairing-card pairing-card--unavailable">
              ${icon("key-round", "empty-symbol")}
              <h2>使用 ID 和固定密码快速查找本机</h2>
              <p>DeskLink 会生成高强度的 8 位密码，并使用当前 Windows 账户加密保存。陌生控制端第一次连接时，仍必须在本机确认。</p>
              <div class="pairing-card-actions">
                <button class="button button--primary" type="button" data-regenerate-fixed-access ${!canEnable || fixedAccessBusy ? "disabled" : ""}>${fixedAccessBusy ? "正在启用…" : "启用固定密码"}</button>
              </div>
            </section>
          `}

      <div class="security-note security-note--pairing">
        ${icon("shield-check", "security-note-mark")}
        <div><strong>固定密码不等于静默授权</strong><p>密码只用于通过中继查找本机。新的控制端仍需通过端到端身份验证，并在这台电脑上获得一次本地批准。</p></div>
      </div>
    </div>
  `;
}

function renderSettings(): string {
  return `
    <div class="page-layout page-layout--settings">
      <header class="page-heading">
        <div>
          <h1>设置 / 诊断</h1>
          <p>管理 DeskLink 的后台运行、脱敏诊断和应用退出。</p>
        </div>
      </header>

      ${preferencesLoading && !preferences
        ? `<div class="settings-loading" aria-label="正在读取 Windows 设置">
            <div class="skeleton skeleton--list"></div>
          </div>`
        : preferences
          ? renderPreferences(preferences)
          : `<section class="settings-unavailable">
              ${icon("circle-alert", "empty-symbol")}
              <h2>无法读取 Windows 设置</h2>
              <p>远程连接不会因此停止，可以重新读取设置。</p>
              <button class="button button--primary" type="button" data-load-preferences>重新读取设置</button>
            </section>`}

      ${snapshot ? renderDiagnosticSummary(snapshot) : ""}
    </div>
  `;
}

function renderPreferences(settings: WindowsPreferencesSummary): string {
  return `
    <section class="settings-group" aria-labelledby="startup-settings-heading">
      <div class="settings-group-heading">
        <h2 id="startup-settings-heading">启动与后台运行</h2>
        <p>这些设置只影响当前 Windows 账户。</p>
      </div>
      <div class="settings-row">
        <div class="settings-row-copy">
          <strong>登录 Windows 后自动启动</strong>
          <p>在后台连接中继，让这台电脑无需手动打开 DeskLink 也能被查找。</p>
        </div>
        <label class="switch-control">
          <input type="checkbox" data-launch-at-login ${settings.launchAtLogin ? "checked" : ""} ${launchAtLoginBusy ? "disabled" : ""}>
          <span aria-hidden="true"></span>
          <b>${launchAtLoginBusy ? "正在保存" : settings.launchAtLogin ? "已开启" : "已关闭"}</b>
        </label>
      </div>
      <div class="settings-row">
        <div class="settings-row-copy">
          <strong>关闭窗口后继续运行</strong>
          <p>点击窗口关闭按钮只会隐藏主界面，远程主机继续在系统托盘运行。</p>
        </div>
        <span class="settings-value">${settings.closeToTray ? "系统托盘" : "直接退出"}</span>
      </div>
    </section>

    <section class="settings-group" aria-labelledby="diagnostics-sharing-heading">
      <div class="settings-group-heading">
        <h2 id="diagnostics-sharing-heading">连接问题诊断</h2>
        <p>仅在你明确开启后，将经过清理的连接状态自动发送到 DeskLink 诊断服务。</p>
      </div>
      <div class="settings-row settings-row--diagnostics">
        <div class="settings-row-copy">
          <strong>共享脱敏诊断</strong>
          <p>不会上传屏幕、按键、访问密码、会话密钥或完整设备身份；记录最多保留 14 天，并在网络恢复后自动补传。</p>
        </div>
        <label class="switch-control">
          <input type="checkbox" data-diagnostics-sharing ${settings.diagnosticsSharingEnabled ? "checked" : ""} ${diagnosticsSharingBusy ? "disabled" : ""}>
          <span aria-hidden="true"></span>
          <b>${diagnosticsSharingBusy ? "正在保存" : settings.diagnosticsSharingEnabled ? "已开启" : "已关闭"}</b>
        </label>
      </div>
      ${settings.diagnosticsSharingEnabled
        ? `<div class="settings-inline-action">
            <div>
              <strong>需要立即排查时</strong>
              <p>点击后发送本机最近的主机端和控制端脱敏事件，重复事件会由服务器自动去重。</p>
            </div>
            <button class="button button--secondary button--compact" type="button" data-upload-diagnostics ${diagnosticsUploadBusy ? "disabled" : ""}>
              ${diagnosticsUploadBusy ? "正在发送…" : "立即发送诊断"}
            </button>
          </div>`
        : ""}
    </section>

    <section class="settings-group" aria-labelledby="update-settings-heading">
      <div class="settings-group-heading">
        <h2 id="update-settings-heading">应用更新</h2>
        <p>只检查 DeskLink GitHub 仓库中已经完成签名验证的正式 Windows 版本。</p>
      </div>
      ${renderWindowsUpdate(settings.version)}
    </section>

    <section class="settings-group" aria-labelledby="application-settings-heading">
      <div class="settings-group-heading">
        <h2 id="application-settings-heading">应用信息</h2>
      </div>
      <dl class="settings-details">
        <div><dt>界面语言</dt><dd>${escapeHtml(settings.interfaceLanguage)}</dd></div>
        <div><dt>当前版本</dt><dd>DeskLink ${escapeHtml(settings.version)}</dd></div>
        <div><dt>数据保护</dt><dd>Windows DPAPI，当前账户专用</dd></div>
      </dl>
    </section>

    <section class="settings-exit" aria-labelledby="exit-settings-heading">
      <div>
        <h2 id="exit-settings-heading">停止后台服务并退出</h2>
        <p>退出后，这台电脑会离线，另一台电脑将无法连接，直到再次启动 DeskLink。</p>
      </div>
      ${quitConfirming
        ? `<div class="settings-exit-confirm" role="group" aria-label="确认退出 DeskLink">
            <strong>确定要让这台电脑离线吗？</strong>
            <button class="button button--secondary" type="button" data-cancel-quit>继续在后台运行</button>
            <button class="button button--danger-quiet" type="button" data-confirm-quit>停止服务并退出</button>
          </div>`
        : '<button class="button button--secondary" type="button" data-request-quit>退出 DeskLink</button>'}
    </section>
  `;
}

function renderWindowsUpdate(currentVersion: string): string {
  let title = `当前版本 DeskLink ${escapeHtml(currentVersion)}`;
  let detail = "尚未检查正式版本。检查不会上传设备 ID、访问密码或远程会话信息。";
  let action = `<button class="button button--secondary button--compact" type="button" data-check-windows-update>${icon("refresh-cw")}检查更新</button>`;

  if (windowsUpdate.kind === "checking") {
    title = "正在检查正式版本";
    detail = "正在读取 GitHub Release 和签名安装器清单，远程连接不会因此暂停。";
    action = `<button class="button button--secondary button--compact" type="button" disabled>${icon("loader-circle")}正在检查…</button>`;
  } else if (windowsUpdate.kind === "available") {
    title = `发现 DeskLink ${escapeHtml(windowsUpdate.latestVersion)}`;
    const releaseDate = windowsUpdate.publishedAt
      ? `，发布于 ${formatUpdateDate(windowsUpdate.publishedAt)}`
      : "";
    detail = `此版本已包含匹配的签名 Windows 安装器清单${releaseDate}。升级会保留设备身份、已批准设备和本机设置。`;
    action = `<button class="button button--primary button--compact" type="button" data-open-windows-release>${icon("file-down")}查看并安装</button>`;
  } else if (windowsUpdate.kind === "current") {
    title = "当前已是最新正式版本";
    detail = `GitHub 最新稳定版本为 DeskLink ${escapeHtml(windowsUpdate.latestVersion)}。`;
    action = `<button class="button button--secondary button--compact" type="button" data-check-windows-update>${icon("refresh-cw")}再次检查</button>`;
  } else if (windowsUpdate.kind === "unavailable") {
    if (windowsUpdate.reason === "noRelease") {
      title = "暂时没有正式 Windows 版本";
      detail = "GitHub 尚未发布通过签名门禁的安装包；当前开发版本可以继续使用。";
    } else if (windowsUpdate.reason === "invalidRelease") {
      title = "最新发布信息无法确认";
      detail = "DeskLink 忽略了不符合稳定版本规则的 Release，不会提供下载入口。";
    } else {
      title = "最新版本尚未通过安装器验证";
      detail = "Release 缺少匹配的 x64 安装器或签名清单，DeskLink 已阻止升级入口。";
    }
    action = `<button class="button button--secondary button--compact" type="button" data-check-windows-update>${icon("refresh-cw")}重新检查</button>`;
  } else if (windowsUpdate.kind === "error") {
    title = "暂时无法检查更新";
    detail = "可能是网络不可用或 GitHub 暂时无法访问，不影响远程连接和本机共享。";
    action = `<button class="button button--secondary button--compact" type="button" data-check-windows-update>${icon("refresh-cw")}重试</button>`;
  }

  return `
    <div class="settings-inline-action settings-update" role="status" aria-live="polite">
      <div>
        <strong>${title}</strong>
        <p>${detail}</p>
      </div>
      ${action}
    </div>
  `;
}

function formatUpdateDate(value: string): string {
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) {
    return "日期未知";
  }
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "long",
    day: "numeric",
  }).format(date);
}

function connectionToInput(connection: ConnectionSummary | null): ConnectionSettingsInput {
  return {
    relayAddress: connection?.relayAddress ?? MANAGED_RELAY_ADDRESS,
    serverName: connection?.serverName ?? MANAGED_RELAY_SERVER_NAME,
    sessionId: connection?.sessionId ?? randomHex(16),
    relayKey: connection ? "" : randomHex(32),
    streamId: String(connection?.streamId ?? 1),
  };
}

function compactIdentifier(value: string): string {
  if (value.length <= 18) {
    return value;
  }
  return `${value.slice(0, 8)}…${value.slice(-8)}`;
}

function formatDate(unixSeconds: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "short",
    day: "numeric",
  }).format(new Date(unixSeconds * 1000));
}

function formatPairingRemaining(expiresAtUnixS: number): string {
  const remainingSeconds = Math.max(0, expiresAtUnixS - Math.floor(Date.now() / 1000));
  if (remainingSeconds === 0) {
    return "已过期";
  }
  const minutes = Math.floor(remainingSeconds / 60);
  const seconds = remainingSeconds % 60;
  return `剩余 ${minutes}:${String(seconds).padStart(2, "0")}`;
}

function formatApprovalRemaining(expiresAtUnixS: number): string {
  const remainingSeconds = Math.max(0, expiresAtUnixS - Math.floor(Date.now() / 1000));
  if (remainingSeconds === 0) {
    return "请求已经失效。";
  }
  const minutes = Math.floor(remainingSeconds / 60);
  const seconds = remainingSeconds % 60;
  return `请求将在 ${minutes}:${String(seconds).padStart(2, "0")} 后失效。`;
}

function clearPairingSecrets(): void {
  if (!pairingSession) {
    return;
  }
  pairingSession.temporaryPassword = "";
}

function clearFixedAccessSecrets(): void {
  if (fixedAccess) {
    fixedAccess.password = "";
  }
  fixedAccess = null;
}

async function loadFixedAccess(): Promise<void> {
  if (!snapshot?.fixedPasswordEnabled || fixedAccessBusy || fixedAccess) {
    return;
  }
  fixedAccessBusy = true;
  render();
  try {
    fixedAccess = await getFixedAccessPassword();
  } catch (error) {
    clearFixedAccessSecrets();
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    fixedAccessBusy = false;
    render();
  }
}

async function regenerateFixedAccess(): Promise<void> {
  if (!snapshot?.connection || fixedAccessBusy) {
    return;
  }
  fixedAccessBusy = true;
  fixedAccessConfirmation = null;
  feedback = { tone: "info", message: snapshot.fixedPasswordEnabled ? "正在更换固定密码…" : "正在启用固定密码…" };
  clearFixedAccessSecrets();
  render();
  try {
    fixedAccess = await regenerateFixedAccessPassword();
    snapshot.fixedPasswordEnabled = true;
    snapshot.fixedPasswordError = null;
    feedback = { tone: "success", message: "固定密码已启用，本机正在向中继发布新的安全访问入口。" };
    await refreshSnapshot(false);
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    fixedAccessBusy = false;
    render();
  }
}

async function disableFixedAccess(): Promise<void> {
  if (!snapshot?.fixedPasswordEnabled || fixedAccessBusy) {
    return;
  }
  fixedAccessBusy = true;
  fixedAccessConfirmation = null;
  clearFixedAccessSecrets();
  feedback = { tone: "info", message: "正在关闭固定密码…" };
  render();
  try {
    snapshot = await disableFixedAccessPassword();
    feedback = { tone: "success", message: "固定密码已关闭，旧密码已经失效。" };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    fixedAccessBusy = false;
    render();
  }
}

async function loadPreferences(): Promise<void> {
  if (preferencesLoading) {
    return;
  }
  preferencesLoading = true;
  render();
  try {
    preferences = await getWindowsPreferences();
  } catch (error) {
    preferences = null;
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    preferencesLoading = false;
    render();
  }
}

async function checkForWindowsUpdate(): Promise<void> {
  if (windowsUpdate.kind === "checking") {
    return;
  }
  const version = applicationVersion || preferences?.version;
  if (!version) {
    windowsUpdate = { kind: "error" };
    if (activeView === "settings") {
      render();
    }
    return;
  }
  windowsUpdate = { kind: "checking" };
  if (activeView === "settings") {
    render();
  }
  try {
    windowsUpdate = evaluateWindowsRelease(version, await checkWindowsRelease());
  } catch {
    windowsUpdate = { kind: "error" };
  } finally {
    if (activeView === "settings") {
      render();
    }
  }
}

async function updateLaunchAtLogin(enabled: boolean): Promise<void> {
  if (launchAtLoginBusy) {
    return;
  }
  launchAtLoginBusy = true;
  feedback = null;
  render();
  try {
    preferences = await setLaunchAtLogin(enabled);
    feedback = {
      tone: "success",
      message: enabled
        ? "已开启登录后自动启动，这台电脑会在登录 Windows 后自动上线。"
        : "已关闭登录后自动启动，下次登录 Windows 后需要手动打开 DeskLink。",
    };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    launchAtLoginBusy = false;
    render();
  }
}

async function updateDiagnosticsSharing(enabled: boolean): Promise<void> {
  if (diagnosticsSharingBusy) {
    return;
  }
  diagnosticsSharingBusy = true;
  feedback = null;
  render();
  try {
    preferences = await setDiagnosticsSharing(enabled);
    feedback = {
      tone: "success",
      message: enabled
        ? "已开启脱敏诊断共享。连接事件会在后台安全发送，网络中断后自动补传。"
        : "已关闭脱敏诊断共享。本机日志仍只保存在当前 Windows 账户中。",
    };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    diagnosticsSharingBusy = false;
    render();
  }
}

async function sendDiagnosticsNow(): Promise<void> {
  if (diagnosticsUploadBusy) {
    return;
  }
  diagnosticsUploadBusy = true;
  feedback = null;
  render();
  try {
    const result = await uploadDiagnosticsNow();
    feedback = {
      tone: "success",
      message: result.uploadedEvents > 0
        ? `已发送 ${result.uploadedEvents} 条脱敏事件，可使用同一连接关联编号排查两台电脑。`
        : "当前没有需要发送的新诊断事件。",
    };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    diagnosticsUploadBusy = false;
    render();
  }
}

async function exitDeskLink(): Promise<void> {
  feedback = { tone: "info", message: "正在停止远程主机并退出 DeskLink。" };
  render();
  try {
    await quitDeskLink();
  } catch (error) {
    quitConfirming = false;
    feedback = { tone: "error", message: normalizeError(error) };
    render();
  }
}

function bindInteractions(): void {
  document.querySelector<HTMLButtonElement>("[data-window-minimize]")?.addEventListener("click", () => {
    runWindowAction(() => applicationWindow.minimize());
  });
  document.querySelector<HTMLButtonElement>("[data-window-maximize]")?.addEventListener("click", () => {
    runWindowAction(() => applicationWindow.toggleMaximize());
  });
  document.querySelector<HTMLButtonElement>("[data-window-close]")?.addEventListener("click", () => {
    runWindowAction(() => applicationWindow.close());
  });
  document.querySelectorAll<HTMLButtonElement>("[data-open-github]").forEach((button) => {
    button.addEventListener("click", () => void openGithub());
  });
  document.querySelector<HTMLButtonElement>("[data-check-windows-update]")?.addEventListener("click", () => {
    void checkForWindowsUpdate();
  });
  document.querySelector<HTMLButtonElement>("[data-open-windows-release]")?.addEventListener("click", () => {
    void openWindowsReleasePage();
  });
  if (activeView === "controller") {
    bindControllerInteractions();
  }
  document.querySelector<HTMLButtonElement>("[data-reject-host-approval]")?.addEventListener("click", () => {
    void answerHostApproval(false);
  });
  document.querySelector<HTMLButtonElement>("[data-allow-host-approval]")?.addEventListener("click", () => {
    void answerHostApproval(true);
  });
  document.querySelector<HTMLElement>(".approval-dialog")?.addEventListener("keydown", handleApprovalKeyboard);
  const navigationButtons = Array.from(
    document.querySelectorAll<HTMLButtonElement>("[data-view]"),
  );
  navigationButtons.forEach((button, currentIndex) => {
    button.addEventListener("click", () => {
      if (activeView === "fixedAccess") {
        clearFixedAccessSecrets();
      }
      fixedAccessConfirmation = null;
      activeView = button.dataset.view as View;
      quitConfirming = false;
      if (activeView === "connection") {
        connectionDraft = null;
        connectionAdvancedOpen = false;
      }
      feedback = null;
      render();
      if (activeView === "fixedAccess") {
        void loadFixedAccess();
      } else if (activeView === "settings" && !preferences) {
        void loadPreferences();
      }
      if (activeView === "settings" && windowsUpdate.kind === "idle") {
        void checkForWindowsUpdate();
      }
    });
    button.addEventListener("keydown", (event) => {
      const nextIndex = nextTabIndex(currentIndex, navigationButtons.length, event.key);
      if (nextIndex === null) {
        return;
      }
      event.preventDefault();
      navigationButtons[nextIndex]?.focus();
      navigationButtons[nextIndex]?.click();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-setup-managed]").forEach((button) => {
    button.addEventListener("click", () => void enableManagedConnection());
  });
  document.querySelectorAll<HTMLButtonElement>("[data-refresh]").forEach((button) => {
    button.addEventListener("click", () => void refreshSnapshot());
  });
  document.querySelector<HTMLButtonElement>("[data-restart-host]")?.addEventListener("click", () => {
    void restartStoppedHost();
  });
  document.querySelector<HTMLButtonElement>("[data-export-diagnostics]")?.addEventListener("click", () => {
    void exportDiagnostics();
  });
  document.querySelector<HTMLButtonElement>("[data-open-connection]")?.addEventListener("click", () => {
    activeView = "connection";
    connectionDraft = null;
    connectionAdvancedOpen = false;
    feedback = null;
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-open-overview]")?.addEventListener("click", () => {
    activeView = "settings";
    feedback = null;
    if (!preferences) {
      void loadPreferences();
    }
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-open-devices]")?.addEventListener("click", () => {
    activeView = "devices";
    feedback = null;
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-open-fixed-access]")?.addEventListener("click", () => {
    activeView = "fixedAccess";
    feedback = null;
    clearFixedAccessSecrets();
    render();
    void loadFixedAccess();
  });
  document.querySelector<HTMLButtonElement>("[data-open-controller]")?.addEventListener("click", () => {
    clearFixedAccessSecrets();
    activeView = "controller";
    feedback = null;
    render();
  });
  document.querySelectorAll<HTMLButtonElement>("[data-start-pairing]").forEach((button) => {
    button.addEventListener("click", () => void beginPairing());
  });
  document.querySelectorAll<HTMLButtonElement>("[data-open-pairing]").forEach((button) => {
    button.addEventListener("click", () => {
      activeView = "pairing";
      feedback = null;
      render();
    });
  });
  document.querySelector<HTMLButtonElement>("[data-copy-host-id]")?.addEventListener("click", () => {
    if (snapshot?.deviceId) {
      void copyCredential(snapshot.deviceId, "host-access-heading", "设备 ID 已复制。");
    }
  });
  document.querySelector<HTMLButtonElement>("[data-copy-device-id]")?.addEventListener("click", () => {
    if (pairingSession) {
      void copyCredential(pairingSession.deviceId, "pairing-device-id", "设备 ID 已复制。");
    }
  });
  document.querySelector<HTMLButtonElement>("[data-copy-temporary-password]")?.addEventListener("click", () => {
    if (pairingSession) {
      void copyCredential(pairingSession.temporaryPassword, "pairing-temporary-password", "临时密码已复制。");
    }
  });
  document.querySelector<HTMLButtonElement>("[data-copy-fixed-device-id]")?.addEventListener("click", () => {
    if (fixedAccess) {
      void copyCredential(fixedAccess.deviceId, "fixed-access-device-id", "设备 ID 已复制。");
    }
  });
  document.querySelector<HTMLButtonElement>("[data-copy-fixed-password]")?.addEventListener("click", () => {
    if (fixedAccess) {
      void copyCredential(fixedAccess.password, "fixed-access-password", "固定密码已复制。");
    }
  });
  document.querySelector<HTMLButtonElement>("[data-regenerate-fixed-access]")?.addEventListener("click", () => {
    if (snapshot?.fixedPasswordEnabled) {
      fixedAccessConfirmation = "regenerate";
      render();
    } else {
      void regenerateFixedAccess();
    }
  });
  document.querySelector<HTMLButtonElement>("[data-disable-fixed-access]")?.addEventListener("click", () => {
    fixedAccessConfirmation = "disable";
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-cancel-fixed-access-action]")?.addEventListener("click", () => {
    fixedAccessConfirmation = null;
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-confirm-fixed-access-action]")?.addEventListener("click", () => {
    const action = fixedAccessConfirmation;
    if (action === "regenerate") {
      void regenerateFixedAccess();
    } else if (action === "disable") {
      void disableFixedAccess();
    }
  });
  document.querySelector<HTMLButtonElement>("[data-cancel-pairing]")?.addEventListener("click", () => {
    void cancelPairing();
  });
  document.querySelectorAll<HTMLButtonElement>("[data-revoke-controller]").forEach((button) => {
    button.addEventListener("click", () => {
      const fingerprint = button.dataset.revokeController;
      if (fingerprint) {
        void revokeController(fingerprint);
      }
    });
  });
  document.querySelector<HTMLButtonElement>("[data-dismiss-feedback]")?.addEventListener("click", () => {
    feedback = null;
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-load-preferences]")?.addEventListener("click", () => {
    void loadPreferences();
  });
  document.querySelector<HTMLInputElement>("[data-launch-at-login]")?.addEventListener("change", (event) => {
    void updateLaunchAtLogin((event.currentTarget as HTMLInputElement).checked);
  });
  document.querySelector<HTMLInputElement>("[data-diagnostics-sharing]")?.addEventListener("change", (event) => {
    void updateDiagnosticsSharing((event.currentTarget as HTMLInputElement).checked);
  });
  document.querySelector<HTMLButtonElement>("[data-upload-diagnostics]")?.addEventListener("click", () => {
    void sendDiagnosticsNow();
  });
  document.querySelector<HTMLButtonElement>("[data-request-quit]")?.addEventListener("click", () => {
    quitConfirming = true;
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-cancel-quit]")?.addEventListener("click", () => {
    quitConfirming = false;
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-confirm-quit]")?.addEventListener("click", () => {
    void exitDeskLink();
  });
  document.querySelector<HTMLButtonElement>("[data-cancel-connection]")?.addEventListener("click", () => {
    connectionDraft = null;
    connectionAdvancedOpen = false;
    activeView = "settings";
    feedback = null;
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-toggle-secret]")?.addEventListener("click", (event) => {
    const button = event.currentTarget as HTMLButtonElement;
    const input = document.querySelector<HTMLInputElement>("#relay-key");
    if (!input) {
      return;
    }
    const showing = input.type === "text";
    input.type = showing ? "password" : "text";
    button.textContent = showing ? "显示" : "隐藏";
    button.setAttribute("aria-label", showing ? "显示中继密钥" : "隐藏中继密钥");
  });
  document.querySelector<HTMLDetailsElement>("[data-connection-advanced]")?.addEventListener("toggle", (event) => {
    connectionAdvancedOpen = (event.currentTarget as HTMLDetailsElement).open;
  });
  document.querySelector<HTMLButtonElement>("[data-generate-connection]")?.addEventListener("click", () => {
    const form = document.querySelector<HTMLFormElement>("[data-connection-form]");
    if (!form) {
      return;
    }
    const data = new FormData(form);
    connectionDraft = {
      relayAddress: String(data.get("relayAddress") ?? ""),
      serverName: String(data.get("serverName") ?? ""),
      sessionId: randomHex(16),
      relayKey: randomHex(32),
      streamId: "1",
    };
    connectionAdvancedOpen = true;
    feedback = { tone: "info", message: "新的连接凭据已生成。保存后，已经配对的电脑需要重新配对。" };
    render();
  });
  document
    .querySelector<HTMLFormElement>("[data-connection-form]")
    ?.addEventListener("submit", (event) => void submitConnection(event));
}

function runWindowAction(action: () => Promise<void>): void {
  void action().catch((error) => {
    feedback = { tone: "error", message: normalizeError(error) };
    render();
  });
}

async function openGithub(): Promise<void> {
  try {
    await openGithubRepository();
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
    render();
  }
}

async function openWindowsReleasePage(): Promise<void> {
  try {
    await openWindowsReleases();
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
    render();
  }
}

async function answerHostApproval(allow: boolean): Promise<void> {
  const approval = snapshot?.pendingApproval;
  const requestId = approval?.requestId;
  if (!requestId || !approval || approvalBusy) {
    return;
  }
  if (approval.expiresAtUnixS <= Math.floor(Date.now() / 1000)) {
    expiredApprovalId = requestId;
    feedback = { tone: "info", message: "此远程控制请求已经过期，请让对方重新发起连接。" };
    render();
    void refreshSnapshot(false);
    return;
  }
  approvalBusy = true;
  render();
  try {
    await respondHostApproval(requestId, allow);
    if (snapshot?.pendingApproval?.requestId === requestId) {
      snapshot.pendingApproval = null;
    }
    feedback = allow
      ? { tone: "success", message: "已允许本次控制，正在建立加密连接。" }
      : { tone: "info", message: "已拒绝本次远程控制请求。" };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    approvalBusy = false;
    focusedApprovalId = null;
    render();
    void refreshSnapshot(false);
  }
}

async function refreshSnapshot(showLoading = true): Promise<void> {
  const request = snapshotRequests.begin();
  loading = showLoading;
  if (showLoading) {
    feedback = null;
    render();
  }
  try {
    const nextSnapshot = await getHostSnapshot();
    if (!snapshotRequests.isCurrent(request)) {
      return;
    }
    snapshot = nextSnapshot;
    if (pairingSession && snapshot.runtime.state === "connected") {
      clearPairingSecrets();
      pairingSession = null;
      if (activeView === "pairing") {
        activeView = "controller";
      }
      feedback = { tone: "success", message: "连接已批准并成功建立。临时密码已从此窗口清除。" };
    } else if (!snapshot.pairingActive && pairingSession) {
      clearPairingSecrets();
      pairingSession = null;
      if (activeView === "pairing") {
        feedback = { tone: "info", message: "上次连接没有完成。你可以重新生成临时密码再试一次。" };
      }
    }
    if (!snapshot.fixedPasswordEnabled || snapshot.fixedPasswordError) {
      clearFixedAccessSecrets();
    }
  } catch (error) {
    if (!snapshotRequests.isCurrent(request)) {
      return;
    }
    if (showLoading || !snapshot) {
      snapshot = null;
    }
    feedback = {
      tone: "error",
      message: showLoading
        ? normalizeError(error)
        : `无法刷新最新状态，界面暂时保留上次结果。${normalizeError(error)}`,
    };
  } finally {
    if (!snapshotRequests.isCurrent(request)) {
      return;
    }
    loading = false;
    if (showLoading || activeView !== "controller") {
      render();
    }
  }
}

async function restartStoppedHost(): Promise<void> {
  if (hostRestartBusy) {
    return;
  }
  hostRestartBusy = true;
  feedback = null;
  render();
  try {
    snapshot = await restartHost();
    feedback = { tone: "info", message: "主机已重新启动，DeskLink 正在连接中继服务器。" };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    hostRestartBusy = false;
    render();
  }
}

async function enableManagedConnection(): Promise<void> {
  if (managedSetupBusy || snapshot?.connection) {
    return;
  }
  managedSetupBusy = true;
  feedback = { tone: "info", message: "正在生成受保护的连接凭据并启用远程连接。" };
  render();
  try {
    snapshot = await setupManagedConnection();
    feedback = {
      tone: "success",
      message: "远程连接已启用。本机现在可以显示设备 ID，并生成临时密码供另一台电脑连接。",
    };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    managedSetupBusy = false;
    render();
  }
}

async function exportDiagnostics(): Promise<void> {
  if (diagnosticExportBusy) {
    return;
  }
  diagnosticExportBusy = true;
  feedback = null;
  render();
  try {
    lastDiagnosticExport = await exportDiagnosticReport();
    feedback = {
      tone: "success",
      message: `诊断报告已导出到 Windows 下载文件夹，报告编号：${lastDiagnosticExport.reportId}。`,
    };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    diagnosticExportBusy = false;
    render();
  }
}

async function beginPairing(): Promise<void> {
  if (!snapshot?.connection || snapshot.trustedError || pairingBusy) {
    return;
  }
  pairingBusy = true;
  feedback = null;
  render();
  try {
    pairingSession = await startPairingSession();
    snapshot.pairingActive = true;
    activeView = "pairing";
  } catch (error) {
    pairingSession = null;
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    pairingBusy = false;
    render();
  }
}

async function cancelPairing(): Promise<void> {
  if (pairingBusy) {
    return;
  }
  pairingBusy = true;
  feedback = null;
  if (pairingSession) {
    clearPairingSecrets();
  }
  pairingSession = null;
  render();
  try {
    snapshot = await cancelPairingSession();
    activeView = "devices";
    feedback = { tone: "success", message: "临时密码已清除，本机在线服务已恢复。" };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    pairingBusy = false;
    render();
  }
}

async function copyCredential(text: string, fallbackElementId: string, successMessage: string): Promise<void> {
  try {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const source = fallbackElementId ? document.getElementById(fallbackElementId) : null;
      if (source instanceof HTMLTextAreaElement || source instanceof HTMLInputElement) {
        source.select();
      } else if (source) {
        const range = document.createRange();
        range.selectNodeContents(source);
        const selection = window.getSelection();
        selection?.removeAllRanges();
        selection?.addRange(range);
      } else {
        throw new Error("此信息已不可用。");
      }
      if (!document.execCommand("copy")) {
        throw new Error("Windows 未允许 DeskLink 复制此信息。");
      }
      window.getSelection()?.removeAllRanges();
    }
    feedback = { tone: "success", message: successMessage };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  }
  render();
}

async function revokeController(fingerprint: string): Promise<void> {
  if (revokingFingerprint) {
    return;
  }
  revokingFingerprint = fingerprint;
  feedback = null;
  render();
  try {
    const result = await revokeTrustedController(fingerprint);
    snapshot = result.snapshot;
    if (!snapshot.pairingActive && pairingSession) {
      clearPairingSecrets();
      pairingSession = null;
    }
    feedback = result.revoked
      ? { tone: "success", message: "控制端访问权限已撤销，主机已使用新的信任列表重新启动。" }
      : { tone: "info", message: "已在 Windows 中取消撤销，控制端访问权限没有改变。" };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    revokingFingerprint = null;
    render();
  }
}

async function submitConnection(event: SubmitEvent): Promise<void> {
  event.preventDefault();
  const form = event.currentTarget as HTMLFormElement;
  if (!form.reportValidity()) {
    return;
  }
  const data = new FormData(form);
  connectionDraft = {
    relayAddress: String(data.get("relayAddress") ?? ""),
    serverName: String(data.get("serverName") ?? ""),
    sessionId: String(data.get("sessionId") ?? ""),
    relayKey: String(data.get("relayKey") ?? ""),
    streamId: String(data.get("streamId") ?? ""),
  };
  saving = true;
  feedback = null;
  render();
  try {
    snapshot = await saveConnectionSettings(connectionDraft);
    connectionDraft.relayKey = "";
    connectionDraft = null;
    connectionAdvancedOpen = false;
    activeView = "controller";
    feedback = { tone: "success", message: "连接设置已保存，并由当前 Windows 账户加密保护。" };
  } catch (error) {
    if (connectionDraft) {
      connectionDraft.relayKey = "";
    }
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    saving = false;
    render();
  }
}

function normalizeError(error: unknown): string {
  if (typeof error === "string") {
    return error;
  }
  if (error instanceof Error) {
    return error.message;
  }
  return "DeskLink 无法完成此本地操作。";
}

function randomHex(byteLength: number): string {
  const bytes = crypto.getRandomValues(new Uint8Array(byteLength));
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

render();
void getVersion().then((version) => {
  applicationVersion = version;
  if (activeView === "about") {
    render();
  }
  void checkForWindowsUpdate();
}).catch(() => {
  applicationVersion = "";
});
void refreshSnapshot();
void initializeController(render);
void listen("host-runtime-changed", () => void refreshSnapshot(false));
void listen("host-approval-changed", () => void refreshSnapshot(false));
window.setInterval(() => {
  const nowUnixS = Math.floor(Date.now() / 1000);
  const approval = snapshot?.pendingApproval;
  const approvalCountdown = document.querySelector<HTMLElement>("[data-approval-countdown]");
  if (approval && approvalCountdown) {
    approvalCountdown.textContent = formatApprovalRemaining(approval.expiresAtUnixS);
    if (approval.expiresAtUnixS <= nowUnixS && expiredApprovalId !== approval.requestId) {
      expiredApprovalId = approval.requestId;
      document.querySelectorAll<HTMLButtonElement>("[data-reject-host-approval], [data-allow-host-approval]")
        .forEach((button) => {
          button.disabled = true;
        });
      feedback = { tone: "info", message: "远程控制请求已过期，对方需要重新发起连接。" };
      void refreshSnapshot(false);
    }
  }
  if (pairingSession && activeView === "pairing") {
    const countdown = document.querySelector<HTMLElement>("[data-pairing-countdown]");
    if (countdown) {
      countdown.textContent = formatPairingRemaining(pairingSession.expiresAtUnixS);
    }
    if (pairingSession.expiresAtUnixS <= nowUnixS) {
      clearPairingSecrets();
      pairingSession = null;
      feedback = { tone: "info", message: "临时密码已过期，本次连接没有完成。请重新生成密码后再试。" };
      render();
      void refreshSnapshot(false);
    }
  }
}, 1000);
