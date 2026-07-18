import "./styles.css";

import { listen } from "@tauri-apps/api/event";

import {
  cancelPairingSession,
  disableFixedAccessPassword,
  exportDiagnosticReport,
  getFixedAccessPassword,
  getHostSnapshot,
  regenerateFixedAccessPassword,
  respondHostApproval,
  restartHost,
  revokeTrustedController,
  saveConnectionSettings,
  setupManagedConnection,
  startPairingSession,
} from "./api";
import type {
  ConnectionSettingsInput,
  ConnectionSummary,
  DiagnosticExportResult,
  FixedAccessSummary,
  HostSnapshot,
  PairingSessionSummary,
  TrustedControllerSummary,
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
import { hostStatusSummary } from "./host-status";

type Feedback = { tone: "success" | "error" | "info"; message: string } | null;

const applicationRoot = document.querySelector<HTMLElement>("#app");
if (!applicationRoot) {
  throw new Error("未找到 DeskLink 应用界面根节点");
}
const app: HTMLElement = applicationRoot;

let snapshot: HostSnapshot | null = null;
let activeView: DeskLinkView = "controller";
let renderedView: DeskLinkView | null = null;
let loading = true;
let saving = false;
let managedSetupBusy = false;
let hostRestartBusy = false;
let approvalBusy = false;
let focusedApprovalId: number | null = null;
let pairingBusy = false;
let pairingSession: PairingSessionSummary | null = null;
let fixedAccess: FixedAccessSummary | null = null;
let fixedAccessBusy = false;
let diagnosticExportBusy = false;
let lastDiagnosticExport: DiagnosticExportResult | null = null;
let revokingFingerprint: string | null = null;
let feedback: Feedback = null;
let connectionDraft: ConnectionSettingsInput | null = null;
let connectionAdvancedOpen = false;
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
      <section class="workspace" aria-busy="${loading}" data-surface-transition="${animateSurface}">
        ${feedback ? renderFeedback(feedback) : ""}
        ${loading ? renderLoading() : renderCurrentView()}
      </section>
    </div>
    ${renderHostApproval()}
  `;
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
      <section class="approval-dialog" role="dialog" aria-modal="true" aria-labelledby="approval-title" aria-describedby="approval-description">
        <div class="approval-heading">
          <span class="approval-icon" aria-hidden="true"></span>
          <div>
            <span class="approval-eyebrow">新的远程控制请求</span>
            <h2 id="approval-title">是否允许这台电脑控制本机？</h2>
          </div>
        </div>
        <p id="approval-description">允许后，对方可以查看你的屏幕，并使用鼠标和键盘。只有确认这是你认识的设备时才允许。</p>
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
        <p class="approval-warning"><span aria-hidden="true">!</span> 如果你没有主动发起连接，请选择“拒绝”。请求会在两分钟内自动失效。</p>
        <div class="approval-actions">
          <button class="button button--secondary" type="button" data-reject-host-approval ${approvalBusy ? "disabled" : ""}>${approvalBusy ? "正在处理…" : "拒绝"}</button>
          <button class="button button--primary" type="button" data-allow-host-approval ${approvalBusy ? "disabled" : ""}>允许本次控制</button>
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
    return;
  }
  if (focusedApprovalId === requestId) {
    return;
  }
  focusedApprovalId = requestId;
  window.requestAnimationFrame(() => {
    document.querySelector<HTMLButtonElement>("[data-reject-host-approval]")?.focus();
  });
}

function renderHeader(): string {
  return `
    <header class="topbar">
      <div class="product-lockup" aria-label="DeskLink Windows 远程桌面">
        <span class="product-mark" aria-hidden="true"><span></span></span>
        <div>
          <strong>DeskLink</strong>
          <span>Windows 远程桌面</span>
        </div>
      </div>
      ${snapshot ? renderHostStatusChip(snapshot) : ""}
    </header>
  `;
}

function renderHostStatusChip(state: HostSnapshot): string {
  const status = hostStatusSummary(state);
  return `<button class="host-status-chip host-status-chip--${status.tone}" type="button" data-open-overview aria-label="${escapeHtml(status.title)}，${escapeHtml(status.detail)}，打开设置 / 诊断">${escapeHtml(status.title)}</button>`;
}

function renderNavigation(): string {
  const activeNavigationView = navigationViewFor(activeView);
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
    </nav>
  `;
}

function renderFeedback(item: NonNullable<Feedback>): string {
  return `
    <div class="feedback feedback--${item.tone}" role="${item.tone === "error" ? "alert" : "status"}" aria-live="${item.tone === "error" ? "assertive" : "polite"}">
      <span class="feedback-symbol" aria-hidden="true"></span>
      <span>${escapeHtml(item.message)}</span>
      <button type="button" class="feedback-close" data-dismiss-feedback aria-label="关闭消息">×</button>
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
    case "overview":
      return renderOverview(snapshot);
    case "controller":
      return renderControllerView();
    case "connection":
      return renderConnection(snapshot);
    case "devices":
      return renderDevices(snapshot);
    case "pairing":
      return renderPairing(snapshot);
    case "fixedAccess":
      return renderFixedAccess(snapshot);
  }
}

function renderFatalState(): string {
  return `
    <div class="empty-state empty-state--error">
      <span class="empty-symbol" aria-hidden="true">!</span>
      <h1>无法读取 DeskLink 状态</h1>
      <p>当前界面无法读取此 Windows 账户的本地状态，主机设置没有被修改。</p>
      <button class="button button--primary" type="button" data-refresh>重新读取</button>
    </div>
  `;
}

function renderOverview(state: HostSnapshot): string {
  const connection = state.connection;
  const statusTitle = connection ? state.runtime.title : state.title;
  const statusDetail = connection ? state.runtime.detail : state.detail;
  const connectionMode = connection
    ? isManagedRelay(connection.relayAddress, connection.serverName)
      ? { value: "DeskLink 公网中继", detail: "支持两台电脑位于不同网络" }
      : { value: "自建中继", detail: connection.serverName }
    : { value: "未配置", detail: "请先启用远程连接" };
  const metrics = [
    {
      label: "连接方式",
      value: connectionMode.value,
      detail: connectionMode.detail,
    },
    {
      label: "Windows 保护",
      value: connection ? "已启用" : "未配置",
      detail: connection ? "连接信息仅当前账户可用" : "保存后自动加密保护",
    },
    {
      label: "已批准设备",
      value: String(state.trustedControllers.length),
      detail: "可以重新连接本机的电脑",
    },
  ];
  return `
    <div class="overview-stack">
      <header class="page-heading overview-heading">
        <div>
          <h1>设置 / 诊断</h1>
          <p>管理本机共享、访问权限和连接诊断。</p>
        </div>
      </header>
      <section class="status-panel status-panel--${state.readiness}" aria-labelledby="status-heading">
        <div class="status-copy">
          <div class="status-label">
            <span class="status-light" aria-hidden="true"></span>
            这台电脑
          </div>
          <h2 id="status-heading">${escapeHtml(statusTitle)}</h2>
          <p>${escapeHtml(statusDetail)}</p>
        </div>
        <div class="status-actions">
          ${
            connection && state.runtime.state === "stopped"
              ? `<button class="button button--primary" type="button" data-restart-host ${hostRestartBusy ? "disabled" : ""} ${hostRestartBusy ? 'aria-busy="true"' : ""}>${hostRestartBusy ? "正在重新启动…" : "重新启动主机"}</button>`
              : ""
          }
          ${
            connection
              ? '<button class="button button--secondary" type="button" data-open-connection>连接设置</button>'
              : `<button class="button button--primary" type="button" data-setup-managed ${managedSetupBusy ? "disabled" : ""} ${managedSetupBusy ? 'aria-busy="true"' : ""}>${managedSetupBusy ? "正在启用…" : "启用远程连接"}</button>
                 <button class="button button--secondary" type="button" data-open-connection ${managedSetupBusy ? "disabled" : ""}>高级设置</button>`
          }
          <button class="button button--secondary" type="button" data-refresh>刷新状态</button>
        </div>
      </section>

      ${renderStateWarnings(state)}

      ${renderHostAccessCard(state)}

      ${renderNextStep(state)}

      <section class="facts" aria-label="主机连接详情">
        ${metrics
          .map(
            (metric) => `
              <div class="fact">
                <span>${metric.label}</span>
                <strong>${escapeHtml(metric.value)}</strong>
                <small>${escapeHtml(metric.detail)}</small>
              </div>
            `,
          )
          .join("")}
      </section>

      ${renderRelayDiagnostics(state)}

      ${renderDiagnosticSummary(state)}

      <section class="recent-access" aria-labelledby="recent-access-heading">
        <div class="section-heading">
          <div>
            <h2 id="recent-access-heading">已批准的访问</h2>
            <p>只有在这台 Windows 设备上批准过的控制端才能重新连接。</p>
          </div>
          <div class="section-actions">
            ${renderPairingAction(state, "text")}
            <button class="text-button" type="button" data-open-devices>管理已批准设备</button>
          </div>
        </div>
        ${renderCompactDeviceList(state.trustedControllers)}
      </section>

      <footer class="refresh-note">
        本地状态刷新于 ${formatTime(state.refreshedAtUnixS)}。中继密钥不会在此窗口中显示。
      </footer>
    </div>
  `;
}

function renderHostAccessCard(state: HostSnapshot): string {
  if (!state.connection || !state.deviceId) {
    return "";
  }
  const available = ["available", "pairing", "connected"].includes(state.runtime.state);
  return `
    <section class="host-access-card" aria-labelledby="host-access-heading">
      <div class="host-access-copy">
        <span class="eyebrow">本机 ID</span>
        <h2 id="host-access-heading">${escapeHtml(state.deviceId)}</h2>
        <p><span class="host-presence ${available ? "host-presence--online" : ""}" aria-hidden="true"></span>${available ? "此设备已在线，可从另一台电脑查找" : "正在恢复在线状态"}</p>
      </div>
      <div class="host-access-actions">
        <button class="button button--secondary" type="button" data-copy-host-id>复制设备 ID</button>
        <button class="button button--secondary" type="button" data-open-fixed-access>${state.fixedPasswordEnabled ? "查看固定密码" : "启用固定密码"}</button>
        ${renderPairingAction(state, "primary")}
      </div>
    </section>
  `;
}

function renderNextStep(state: HostSnapshot): string {
  if (state.trustedControllers.length > 0) {
    return "";
  }
  const approvalStoreUnavailable = Boolean(state.trustedError);
  const stage = !state.connection ? 1 : state.pairingActive ? 3 : 2;
  const title = !state.connection
    ? "启用这台电脑的远程连接"
    : approvalStoreUnavailable
      ? "先恢复已批准设备列表"
    : state.pairingActive
        ? "在另一台电脑输入 ID 和临时密码"
        : "生成第一份临时密码";
  const detail = !state.connection
    ? "DeskLink 会生成受保护的连接凭据并使用已部署的公网中继，无需填写网络参数。"
    : approvalStoreUnavailable
      ? "DeskLink 暂时无法安全读取已批准设备，请重新读取设置 / 诊断状态后再创建连接码。"
    : state.pairingActive
        ? "打开另一台电脑的“连接设备”页面，输入本机 ID 和临时密码。"
        : "临时密码有效时间很短；找到本机后，仍需在这台电脑上确认连接请求。";
  const action = !state.connection
    ? `<button class="button button--primary" type="button" data-setup-managed ${managedSetupBusy ? "disabled" : ""}>${managedSetupBusy ? "正在启用…" : "启用远程连接"}</button>`
    : approvalStoreUnavailable
      ? '<button class="button button--primary" type="button" data-refresh>重新读取设置 / 诊断状态</button>'
    : state.pairingActive
      ? '<button class="button button--primary" type="button" data-open-pairing>查看临时密码</button>'
      : '<button class="button button--primary" type="button" data-start-pairing>生成临时密码</button>';
  return `
    <section class="next-step" aria-labelledby="next-step-heading">
      <div class="next-step-copy">
        <span>建议下一步</span>
        <h2 id="next-step-heading">${title}</h2>
        <p>${detail}</p>
      </div>
      <ol class="setup-progress" aria-label="首次连接进度">
        <li class="${stage > 1 ? "is-complete" : "is-current"}"><span>1</span><strong>启用远程连接</strong></li>
        <li class="${stage > 2 ? "is-complete" : stage === 2 ? "is-current" : ""}"><span>2</span><strong>生成临时密码</strong></li>
        <li class="${stage === 3 ? "is-current" : ""}"><span>3</span><strong>在本机批准</strong></li>
      </ol>
      <div class="next-step-action">${action}</div>
    </section>
  `;
}

function renderRelayDiagnostics(state: HostSnapshot): string {
  const relay = state.relayStatus;
  if (relay.mode === "unconfigured") {
    return "";
  }
  return `
    <section class="relay-diagnostics" aria-labelledby="relay-diagnostics-heading">
      <div class="section-heading">
        <div>
          <h2 id="relay-diagnostics-heading">中继连接方式</h2>
          <p>${escapeHtml(relay.detail)}</p>
        </div>
        <span class="relay-health relay-health--${relay.state}">
          <span aria-hidden="true"></span>已配置
        </span>
      </div>
    </section>
  `;
}

function renderDiagnosticSummary(state: HostSnapshot): string {
  const failed = state.diagnosticChecks.filter((check) => check.status === "failed").length;
  const warning = state.diagnosticChecks.filter((check) => check.status === "warning").length;
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
              <span aria-hidden="true">!</span>
              <p>${escapeHtml(warning)}</p>
            </div>
          `,
        )
        .join("")}
    </div>
  `;
}

function renderCompactDeviceList(devices: TrustedControllerSummary[]): string {
  if (devices.length === 0) {
    return `
      <div class="empty-row">
        <span class="empty-row-symbol" aria-hidden="true"></span>
        <div>
          <strong>暂无可信控制端</strong>
          <p>在这台电脑上批准控制端后，它才会显示在这里。</p>
        </div>
      </div>
    `;
  }
  return `
    <div class="compact-device-list">
      ${devices
        .slice(0, 2)
        .map(
          (device) => `
            <div class="compact-device">
              <span class="device-avatar" aria-hidden="true"></span>
              <div>
                <strong>${escapeHtml(compactIdentifier(device.deviceId))}</strong>
                <span>批准于 ${formatDate(device.approvedAtUnixS)}</span>
              </div>
              <code>${escapeHtml(compactIdentifier(device.fingerprint))}</code>
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
          <p>保存后，这台电脑才能创建连接码并等待另一台电脑连接。</p>
        </div>
        <div class="page-actions">
          <span class="storage-note">由 Windows DPAPI 加密保护</span>
        </div>
      </header>

      ${state.connectionError ? renderStateWarnings(state) : ""}

      <div class="connection-guidance share-device-card">
        <span class="connection-guidance-mark" aria-hidden="true"></span>
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
        <span class="security-note-mark" aria-hidden="true"></span>
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
      <span class="empty-device" aria-hidden="true"></span>
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
        <span class="device-avatar" aria-hidden="true"></span>
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
  const displayedInvitation = session?.invitation ?? "";
  return `
    <div class="page-layout page-layout--pairing">
      <header class="page-heading page-heading--pairing">
        <div>
          <button class="back-button" type="button" data-open-overview aria-label="返回设置 / 诊断">← 设置 / 诊断</button>
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
            <section class="pairing-card" aria-labelledby="pairing-invitation-heading">
              <div class="pairing-card-heading">
                <div>
                  <span class="eyebrow">本次连接凭据</span>
                  <h2 id="pairing-invitation-heading">输入到另一台电脑</h2>
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
              <details class="pairing-legacy-panel">
                <summary>旧版 DeskLink 连接码</summary>
                <div class="pairing-legacy-content">
                  <p>只有另一台电脑尚未升级、没有设备 ID 输入框时才需要使用。</p>
                  <label class="sr-only" for="pairing-invitation">旧版配对连接码</label>
                  <textarea id="pairing-invitation" class="pairing-invitation" readonly spellcheck="false">${escapeHtml(displayedInvitation)}</textarea>
                  <button class="button button--secondary button--compact" type="button" data-copy-pairing ${pairingBusy ? "disabled" : ""}>复制旧版连接码</button>
                </div>
              </details>
              <p class="secret-note" id="pairing-secret-note">临时密码仅供本次连接使用，请勿发送到公开聊天或工单。</p>
            </section>
          `
          : `
            <section class="pairing-card pairing-card--unavailable">
              <span class="empty-symbol" aria-hidden="true">${active ? "…" : "×"}</span>
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
        <span class="security-note-mark" aria-hidden="true"></span>
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
          <button class="back-button" type="button" data-open-overview aria-label="返回设置 / 诊断">← 设置 / 诊断</button>
          <h1>固定访问密码</h1>
          <p>适合经常从自己的另一台电脑连接本机，无需每次重新生成临时密码。</p>
        </div>
        <span class="pairing-state ${enabled ? "pairing-state--active" : ""}">
          <span aria-hidden="true"></span>${enabled ? "固定密码已启用" : "固定密码未启用"}
        </span>
      </header>

      ${state.fixedPasswordError
        ? `<section class="pairing-card pairing-card--unavailable"><span class="empty-symbol" aria-hidden="true">!</span><h2>无法读取固定密码</h2><p>${escapeHtml(state.fixedPasswordError)}</p></section>`
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
                  <button class="button button--secondary" type="button" data-regenerate-fixed-access ${fixedAccessBusy ? "disabled" : ""}>${fixedAccessBusy ? "正在更换…" : "更换固定密码"}</button>
                  <button class="button button--danger-quiet" type="button" data-disable-fixed-access ${fixedAccessBusy ? "disabled" : ""}>关闭固定密码</button>
                </div>
                <p class="secret-note">更换或关闭后，旧固定密码会立即失效；已批准设备仍可使用保存的安全连接。</p>
              </section>
            `
            : `<section class="pairing-card pairing-card--unavailable"><span class="controller-spinner" aria-hidden="true"></span><h2>正在读取固定密码</h2><p>密码只会在你主动打开此页面时解密显示。</p></section>`
          : `
            <section class="pairing-card pairing-card--unavailable">
              <span class="empty-symbol" aria-hidden="true">•••</span>
              <h2>使用 ID 和固定密码快速查找本机</h2>
              <p>DeskLink 会生成高强度的 8 位密码，并使用当前 Windows 账户加密保存。陌生控制端第一次连接时，仍必须在本机确认。</p>
              <div class="pairing-card-actions">
                <button class="button button--primary" type="button" data-regenerate-fixed-access ${!canEnable || fixedAccessBusy ? "disabled" : ""}>${fixedAccessBusy ? "正在启用…" : "启用固定密码"}</button>
              </div>
            </section>
          `}

      <div class="security-note security-note--pairing">
        <span class="security-note-mark" aria-hidden="true"></span>
        <div><strong>固定密码不等于静默授权</strong><p>密码只用于通过中继查找本机。新的控制端仍需通过端到端身份验证，并在这台电脑上获得一次本地批准。</p></div>
      </div>
    </div>
  `;
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

function formatTime(unixSeconds: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "numeric",
    minute: "2-digit",
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

function clearPairingSecrets(): void {
  if (!pairingSession) {
    return;
  }
  pairingSession.temporaryPassword = "";
  pairingSession.invitation = "";
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
  if (snapshot.fixedPasswordEnabled
    && !window.confirm("更换固定密码后，旧固定密码会立即失效。要继续吗？")) {
    return;
  }
  fixedAccessBusy = true;
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
  if (!snapshot?.fixedPasswordEnabled || fixedAccessBusy
    || !window.confirm("关闭固定密码后，其他电脑将无法再用它查找本机。要继续吗？")) {
    return;
  }
  fixedAccessBusy = true;
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

function bindInteractions(): void {
  if (activeView === "controller") {
    bindControllerInteractions();
  }
  document.querySelector<HTMLButtonElement>("[data-reject-host-approval]")?.addEventListener("click", () => {
    void answerHostApproval(false);
  });
  document.querySelector<HTMLButtonElement>("[data-allow-host-approval]")?.addEventListener("click", () => {
    void answerHostApproval(true);
  });
  const navigationButtons = Array.from(
    document.querySelectorAll<HTMLButtonElement>("[data-view]"),
  );
  navigationButtons.forEach((button, currentIndex) => {
    button.addEventListener("click", () => {
      if (activeView === "fixedAccess") {
        clearFixedAccessSecrets();
      }
      activeView = button.dataset.view as DeskLinkView;
      if (activeView === "connection") {
        connectionDraft = null;
        connectionAdvancedOpen = false;
      }
      feedback = null;
      render();
      if (activeView === "fixedAccess") {
        void loadFixedAccess();
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
  document.querySelector<HTMLButtonElement>("[data-open-overview]")?.addEventListener("click", () => {
    clearFixedAccessSecrets();
    activeView = "overview";
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
  document.querySelector<HTMLButtonElement>("[data-copy-pairing]")?.addEventListener("click", () => {
    void copyPairingInvitation();
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
    void regenerateFixedAccess();
  });
  document.querySelector<HTMLButtonElement>("[data-disable-fixed-access]")?.addEventListener("click", () => {
    void disableFixedAccess();
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
  document.querySelector<HTMLButtonElement>("[data-cancel-connection]")?.addEventListener("click", () => {
    connectionDraft = null;
    connectionAdvancedOpen = false;
    activeView = "overview";
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

async function answerHostApproval(allow: boolean): Promise<void> {
  const requestId = snapshot?.pendingApproval?.requestId;
  if (!requestId || approvalBusy) {
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
        activeView = "overview";
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
    snapshot = null;
    feedback = {
      tone: "error",
      message: normalizeError(error),
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

async function copyPairingInvitation(): Promise<void> {
  if (!pairingSession || !snapshot) {
    return;
  }
  await copyCredential(
    pairingSession.invitation,
    "pairing-invitation",
    "旧版连接码已复制，请在另一台 DeskLink 电脑的兼容入口粘贴。",
  );
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
    activeView = "overview";
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
void refreshSnapshot();
void initializeController(render);
void listen("host-runtime-changed", () => void refreshSnapshot(false));
void listen("host-approval-changed", () => void refreshSnapshot(false));
window.setInterval(() => {
  if (!pairingSession || activeView !== "pairing") {
    return;
  }
  const countdown = document.querySelector<HTMLElement>("[data-pairing-countdown]");
  if (countdown) {
    countdown.textContent = formatPairingRemaining(pairingSession.expiresAtUnixS);
  }
  if (pairingSession.expiresAtUnixS <= Math.floor(Date.now() / 1000)) {
    clearPairingSecrets();
    pairingSession = null;
    feedback = { tone: "info", message: "临时密码已过期，本次连接没有完成。请重新生成密码后再试。" };
    render();
    void refreshSnapshot(false);
  }
}, 1000);
