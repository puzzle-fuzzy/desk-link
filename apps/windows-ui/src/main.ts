import "./styles/layout.css";
import "./styles/base.css";
import "./styles/interaction.css";
import "./styles/file-queue.css";
import "./styles/session-surface.css";

import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";

import {
  accountForgotPassword,
  accountCancelOperation,
  accountAcceptOperation,
  accountFinishOperation,
  accountLogin,
  accountLogout,
  accountRegister,
  accountResendVerification,
  accountRestore,
  cancelPairingSession,
  cancelWindowsUpdate,
  checkWindowsRelease,
  disableFixedAccessPassword,
  exportDiagnosticReport,
  getFixedAccessPassword,
  getHostSnapshot,
  getWindowsPreferences,
  installWindowsUpdate,
  openGithubRepository,
  quitDeskLink,
  regenerateFixedAccessPassword,
  respondHostApproval,
  restartHost,
  pauseHostSharing,
  resumeHostSharing,
  revokeTrustedController,
  saveConnectionSettings,
  setupManagedConnection,
  setDiagnosticsSharing,
  setLaunchAtLogin,
  startPairingSession,
  uploadDiagnosticsNow,
} from "./api";
import type { AccountSnapshot } from "./api";
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
  controllerSurfaceIdentity,
  updatePersistentControllerRegions,
} from "./controller";
import { retainSessionSurface } from "./session-surface";
import {
  MANAGED_RELAY_ADDRESS,
  MANAGED_RELAY_SERVER_NAME,
} from "./product-config";
import {
  DESKTOP_NAV_ITEMS,
  navigationViewFor,
  nextMenuIndex,
  nextTabIndex,
  type DeskLinkView,
} from "./navigation";
import { LatestRequest } from "./latest-request";
import { RenderScheduler } from "./render-scheduler";
import {
  setAccountLocalModeEnabled,
  canRecoverWorkspaceSnapshot,
  shouldStartInLocalMode,
} from "./account-mode";
import { escapeHtml } from "./html";
import { formatDeviceId } from "./device-credentials";
import { icon, renderLucideIcons } from "./icons";
import { hostStatusSummary } from "./host-status";
import { hostSnapshotPresentationChanged } from "./host-snapshot-presentation";
import { approvalNeedsFocus, approvalTabFocusIndex } from "./approval-focus";
import { WindowsUpdater, windowsUpdateProgressLabel } from "./windows-update";
import { AccountFormDraft } from "./account-form-draft";
import { AccountOperations } from "./account-operation";

type View = "controller" | "connection" | "devices" | "pairing" | "fixedAccess" | "settings" | "about";
type Feedback = { tone: "success" | "error" | "info"; message: string } | null;
const applicationRoot = document.querySelector<HTMLElement>("#app");
if (!applicationRoot) {
  throw new Error("未找到 DeskLink 应用界面根节点");
}
const app: HTMLElement = applicationRoot;
const windowsUpdateBanner = document.createElement("aside");
windowsUpdateBanner.className = "windows-update-banner";
windowsUpdateBanner.dataset.windowsUpdateRegion = "banner";
windowsUpdateBanner.hidden = true;
windowsUpdateBanner.setAttribute("aria-label", "应用更新");
app.before(windowsUpdateBanner);
const windowsUpdateBoundRegions = new WeakSet<HTMLElement>();
const renderScheduler = new RenderScheduler(
  (callback) => window.requestAnimationFrame(callback),
  (handle) => window.cancelAnimationFrame(handle),
);

let snapshot: HostSnapshot | null = null;
let activeView: View = "controller";
let renderedView: View | null = null;
let renderedControllerSurface: string | null = null;
let loading = true;
let saving = false;
let managedSetupBusy = false;
let hostRestartBusy = false;
let hostSharingBusy = false;
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
const windowsUpdater = new WindowsUpdater({
  version: async () => {
    applicationVersion = applicationVersion || preferences?.version || await getVersion();
    return applicationVersion;
  },
  check: checkWindowsRelease,
  install: installWindowsUpdate,
  cancel: cancelWindowsUpdate,
  changed: updateWindowsUpdateRegions,
});
let utilityMenuOpen = false;
let utilityMenuDismissBound = false;
const snapshotRequests = new LatestRequest();
const fixedAccessRequests = new LatestRequest();
let account: AccountSnapshot = { signedIn: false, user: null, deviceId: null };
let accountLocalMode = false;
let accountLoading = true;
let accountError: string | null = null;
let accountMode: "login" | "register" | "forgot" = "login";
let accountBusy = false;
let accountNotice: string | null = null;
let accountCanResendVerification = false;
const accountDraft = new AccountFormDraft();
const accountOperations = new AccountOperations();
let accountErrorField: "email" | "password" | "confirmation" | null = null;
let snapshotRecoveryTimer: number | null = null;
const workspaceHostEvents = ["host-runtime-changed", "host-approval-changed"] as const;
const workspaceHostListeners = new Set<string>();
const workspaceHostListenerAttempts = new Map<string, Promise<void>>();
let workspaceRecoveryBound = false;
let snapshotReadFeedback: Feedback = null;

function render(): void {
  if (accountLoading) {
    prepareControllerRender();
    renderedControllerSurface = null;
    app.innerHTML = renderAccountLoading();
    return;
  }
  if (!account.signedIn && !accountLocalMode) {
    prepareControllerRender();
    renderedControllerSurface = null;
    app.innerHTML = renderAccountGate();
    bindAccountInteractions();
    return;
  }
  const nextSurface = activeView === "controller" && !loading ? controllerSurfaceIdentity() : null;
  if (retainSessionSurface(renderedControllerSurface, nextSurface, renderedView === activeView, Boolean(app.querySelector("[data-remote-viewport]")))) {
    patchPersistentSessionShell();
    return;
  }
  prepareControllerRender();
  const previousWorkspace = document.querySelector<HTMLElement>(".workspace");
  const preservedScrollTop = renderedView === activeView ? (previousWorkspace?.scrollTop ?? 0) : 0;
  const animateSurface = renderedView !== activeView;
  app.innerHTML = `
    <div class="app-shell">
      ${renderSidebar()}
      <div class="app-main">
        ${renderWorkspaceTopbar()}
        <main class="workspace ${activeView === "controller" ? "workspace--controller" : ""}" aria-busy="${loading}" data-surface-transition="${animateSurface}">
          <div data-shell-feedback>${feedback ? renderFeedback(feedback) : ""}</div>
          ${loading ? renderLoading() : renderCurrentView()}
        </main>
      </div>
    </div>
    <div data-host-approval-region>${renderHostApproval()}</div>
  `;
  renderLucideIcons(app);
  const currentWorkspace = document.querySelector<HTMLElement>(".workspace");
  if (currentWorkspace && preservedScrollTop > 0) {
    currentWorkspace.scrollTop = preservedScrollTop;
  }
  renderedView = activeView;
  renderedControllerSurface = nextSurface;
  bindInteractions();
  bindUtilityMenuDismiss();
  focusNewApproval();
  updateWindowsUpdateRegions();
}

function patchPersistentSessionShell(): void {
  const regions: Array<[string, string]> = [
    [".sidebar", renderSidebar()],
    [".workspace-topbar", renderWorkspaceTopbar()],
  ];
  for (const [selector, markup] of regions) {
    const previous = app.querySelector<HTMLElement>(selector);
    if (previous) previous.outerHTML = markup;
  }
  const feedbackRegion = app.querySelector<HTMLElement>("[data-shell-feedback]");
  if (feedbackRegion) feedbackRegion.innerHTML = feedback ? renderFeedback(feedback) : "";
  const approvalRegion = app.querySelector<HTMLElement>("[data-host-approval-region]");
  if (approvalRegion) approvalRegion.innerHTML = renderHostApproval();
  updatePersistentControllerRegions();
  renderLucideIcons(app);
  bindInteractions(true);
  bindUtilityMenuDismiss();
  focusNewApproval();
  updateWindowsUpdateRegions();
}

function scheduleRender(): void {
  renderScheduler.schedule(render);
}

function renderAccountLoading(): string {
  return `<main class="account-gate"><section class="account-card account-card--loading" role="status"><strong>正在加载…</strong></section></main>`;
}

function renderAccountGate(): string {
  const title = accountMode === "login" ? "登录 DeskLink" : accountMode === "register" ? "创建 DeskLink 账号" : "找回密码";
  const action = accountMode === "login" ? "登录" : accountMode === "register" ? "注册并发送验证邮件" : "发送重置邮件";
  return `
    <main class="account-gate">
      <section class="account-card" aria-labelledby="account-title">
        <h1 id="account-title">${title}</h1>
        <div class="account-mode-switch" role="tablist" aria-label="账号操作">
          <button id="account-tab-login" role="tab" aria-selected="${accountMode === "login"}" aria-controls="account-form-panel" tabindex="${accountMode === "login" ? "0" : "-1"}" type="button" data-account-mode="login" class="${accountMode === "login" ? "is-active" : ""}">登录</button>
          <button id="account-tab-register" role="tab" aria-selected="${accountMode === "register"}" aria-controls="account-form-panel" tabindex="${accountMode === "register" ? "0" : "-1"}" type="button" data-account-mode="register" class="${accountMode === "register" ? "is-active" : ""}">注册</button>
          <button id="account-tab-forgot" role="tab" aria-selected="${accountMode === "forgot"}" aria-controls="account-form-panel" tabindex="${accountMode === "forgot" ? "0" : "-1"}" type="button" data-account-mode="forgot" class="${accountMode === "forgot" ? "is-active" : ""}">找回密码</button>
        </div>
        <form id="account-form-panel" role="tabpanel" aria-labelledby="account-tab-${accountMode}" data-account-form>
          <label class="account-field"><span>邮箱</span><input name="email" type="email" autocomplete="email" placeholder="name@example.com" required></label>
          ${accountMode === "forgot" ? "" : `<label class="account-field"><span>密码</span><input name="password" type="password" autocomplete="${accountMode === "login" ? "current-password" : "new-password"}" minlength="12" maxlength="128" placeholder="至少 12 个字符" required></label>`}
          ${accountMode === "register" ? `<label class="account-field"><span>确认密码</span><input name="confirmation" type="password" autocomplete="new-password" minlength="12" maxlength="128" placeholder="再次输入密码" required></label>` : ""}
          ${accountError ? `<p class="account-error" role="alert">${escapeHtml(accountError)}</p>` : ""}
          ${accountNotice ? `<p class="account-notice" role="status">${escapeHtml(accountNotice)}</p>${accountCanResendVerification ? `<button class="text-button account-resend" type="button" data-resend-verification ${accountBusy ? "disabled" : ""}>重新发送验证邮件</button>` : ""}` : ""}
          <button class="button button--primary account-submit" type="submit" ${accountBusy ? "disabled aria-busy=\"true\"" : ""}>${accountBusy ? "正在处理…" : action}</button>
        </form>
        <button class="button button--secondary account-local-mode" type="button" data-account-local-mode>跳过登录，直接使用</button>
      </section>
    </main>
  `;
}

function bindAccountInteractions(): void {
  const accountForm = document.querySelector<HTMLFormElement>("[data-account-form]");
  if (accountForm) {
    accountDraft.restore(accountForm);
    accountForm.addEventListener("input", () => accountDraft.capture(new FormData(accountForm)));
    if (accountErrorField) {
      const input = accountForm.querySelector<HTMLInputElement>(`input[name="${accountErrorField}"]`);
      input?.setAttribute("aria-invalid", "true");
      input?.focus();
      accountErrorField = null;
    }
  }
  const accountTabs = Array.from(
    document.querySelectorAll<HTMLButtonElement>('[role="tab"][data-account-mode]'),
  );
  const selectAccountMode = (button: HTMLButtonElement): void => {
    const mode = button.dataset.accountMode;
    if (mode !== "login" && mode !== "register" && mode !== "forgot") return;
    invalidateAccountOperation();
    accountMode = mode;
    accountDraft.clearSecrets();
    accountError = null;
    accountNotice = null;
    accountCanResendVerification = false;
    render();
    window.requestAnimationFrame(() => {
      document.querySelector<HTMLButtonElement>(`[data-account-mode="${mode}"]`)?.focus();
    });
  };
  accountTabs.forEach((button, currentIndex) => {
    button.addEventListener("click", () => {
      selectAccountMode(button);
    });
    button.addEventListener("keydown", (event) => {
      const nextIndex = nextTabIndex(currentIndex, accountTabs.length, event.key);
      if (nextIndex === null) return;
      event.preventDefault();
      const nextTab = accountTabs[nextIndex];
      if (nextTab) selectAccountMode(nextTab);
    });
  });
  document.querySelector<HTMLFormElement>("[data-account-form]")?.addEventListener("submit", (event) => {
    event.preventDefault();
    void submitAccount(event.currentTarget as HTMLFormElement);
  });
  document.querySelector<HTMLButtonElement>("[data-resend-verification]")?.addEventListener("click", () => {
    void resendVerification();
  });
  document.querySelector<HTMLButtonElement>("[data-account-local-mode]")?.addEventListener("click", () => {
    void enterAccountLocalMode();
  });
}

async function enterAccountLocalMode(): Promise<void> {
  const cancelled = invalidateAccountOperation();
  const operation = accountOperations.begin("local");
  accountDraft.leave();
  accountLocalMode = true;
  accountError = null;
  accountNotice = null;
  setAccountLocalModeEnabled(true);
  render();
  try {
    await cancelled;
    if (!accountOperations.current(operation)) return;
    await restartHost();
    if (!accountOperations.current(operation)) return;
    await startWorkspace();
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
    render();
  }
}

async function resendVerification(): Promise<void> {
  if (accountBusy) return;
  const email = accountDraft.pendingVerificationEmail;
  if (!email) {
    accountError = "请先填写注册邮箱。";
    render();
    return;
  }
  accountBusy = true;
  const operation = accountOperations.begin("resend");
  accountError = null;
  render();
  try {
    await accountResendVerification(email, operation.generation);
    if (!accountOperations.current(operation)) return;
    accountNotice = "验证邮件已重新发送，请检查收件箱。";
  } catch (error) {
    if (!accountOperations.current(operation)) return;
    accountError = normalizeError(error);
  } finally {
    if (accountOperations.current(operation)) {
      accountBusy = false;
      render();
    }
  }
}

function invalidateAccountOperation(): Promise<void> {
  accountBusy = false;
  const generation = accountOperations.invalidate();
  return accountCancelOperation(generation).catch((error) => {
    if (accountOperations.current({ generation, action: "cancel" })) accountError = normalizeError(error);
  });
}

async function submitAccount(form: HTMLFormElement): Promise<void> {
  if (accountBusy) return;
  const data = new FormData(form);
  accountDraft.capture(data);
  const email = String(data.get("email") ?? "").trim();
  const password = String(data.get("password") ?? "");
  const validation = accountDraft.validate(accountMode);
  if (validation) {
    accountError = validation.message;
    accountErrorField = validation.field;
    render();
    return;
  }
  accountBusy = true;
  const operation = accountOperations.begin(accountMode);
  accountError = null;
  accountNotice = null;
  render();
  try {
    if (operation.action === "login") {
      const result = await accountLogin(email, password, operation.generation);
      if (!accountOperations.current(operation)) return;
      if (result.signedIn) {
        await accountAcceptOperation(operation.generation);
        if (!accountOperations.current(operation)) return;
        await accountFinishOperation(operation.generation);
        if (!accountOperations.current(operation)) return;
        account = result;
        accountDraft.leave();
        accountLocalMode = false;
        setAccountLocalModeEnabled(false);
        await startWorkspace();
      }
    } else if (operation.action === "register") {
      await accountRegister(email, password, operation.generation);
      if (!accountOperations.current(operation)) return;
      accountDraft.registered(email);
      accountMode = "login";
      accountNotice = "验证邮件已发送，请完成邮箱验证后再登录。";
      accountCanResendVerification = true;
    } else {
      await accountForgotPassword(email, operation.generation);
      if (!accountOperations.current(operation)) return;
      accountDraft.clearSecrets();
      accountMode = "login";
      accountNotice = "如果邮箱已注册，重置密码邮件会很快送达。";
      accountCanResendVerification = false;
    }
  } catch (error) {
    if (!accountOperations.current(operation)) return;
    accountError = normalizeError(error);
  } finally {
    if (accountOperations.current(operation)) {
      accountBusy = false;
      render();
    }
  }
}

async function logoutAccount(): Promise<void> {
  if (accountBusy) return;
  accountBusy = true;
  accountDraft.leave();
  clearFixedAccessSecrets();
  clearPairingSecrets();
  pairingSession = null;
  render();
  try {
    await accountLogout();
    account = { signedIn: false, user: null, deviceId: null };
    accountLocalMode = false;
    setAccountLocalModeEnabled(false);
    if (snapshotRecoveryTimer !== null) {
      window.clearTimeout(snapshotRecoveryTimer);
      snapshotRecoveryTimer = null;
    }
    snapshot = null;
    activeView = "controller";
    renderedView = null;
    accountMode = "login";
    accountNotice = "已退出登录，本机远程连接记录已清除。";
  } catch (error) {
    accountError = normalizeError(error);
  } finally {
    accountBusy = false;
    render();
  }
}

function bindUtilityMenuDismiss(): void {
  if (utilityMenuDismissBound) {
    return;
  }
  document.addEventListener("pointerdown", (event) => {
    if (!utilityMenuOpen) {
      return;
    }
    const target = event.target;
    if (target instanceof Element && (target.closest("[data-toggle-utility-menu]") || target.closest(".utility-menu"))) {
      return;
    }
    // Keep keyboard users anchored to the control that opened the menu when
    // an empty area dismisses it. Do not steal focus when the user is moving
    // directly to another navigation action (the click handler will focus or
    // render that destination itself).
    const restoreToggleFocus = !(target instanceof Element && target.closest("[data-view]"));
    utilityMenuOpen = false;
    render();
    if (restoreToggleFocus) {
      window.requestAnimationFrame(() => {
        document.querySelector<HTMLButtonElement>("[data-toggle-utility-menu]")?.focus();
      });
    }
  });
  document.addEventListener("keydown", (event) => {
    if (!utilityMenuOpen || event.key !== "Escape") {
      return;
    }
    event.preventDefault();
    utilityMenuOpen = false;
    render();
    window.requestAnimationFrame(() => {
      document.querySelector<HTMLButtonElement>("[data-toggle-utility-menu]")?.focus();
    });
  });
  utilityMenuDismissBound = true;
}

function handleUtilityMenuKeyboard(event: KeyboardEvent): void {
  const menu = event.currentTarget as HTMLElement;
  const items = Array.from(menu.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'));
  const currentIndex = items.indexOf(document.activeElement as HTMLButtonElement);
  const nextIndex = nextMenuIndex(currentIndex, items.length, event.key);
  if (nextIndex === null) {
    return;
  }
  event.preventDefault();
  items[nextIndex]?.focus();
}

function renderHostApproval(): string {
  const approval = snapshot?.pendingApproval;
  if (!approval) {
    return "";
  }
  const actionsDisabled = approvalBusy || approval.expiresAtUnixS <= Math.floor(Date.now() / 1000);
  return `
    <div class="approval-backdrop">
      <section class="approval-dialog" role="dialog" aria-modal="true" aria-labelledby="approval-title" aria-describedby="approval-description approval-warning" tabindex="-1">
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
          <button class="button button--secondary" type="button" data-reject-host-approval ${actionsDisabled ? "disabled" : ""}>${approvalBusy ? "正在处理…" : "拒绝"}</button>
          <button class="button button--primary" type="button" data-allow-host-approval ${actionsDisabled ? "disabled" : ""}>${approval.identityChanged ? "确认并替换旧身份" : "允许本次控制"}</button>
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
  const dialog = document.querySelector<HTMLElement>(".approval-dialog");
  const activeElement = document.activeElement;
  const focusInsideDialog = Boolean(dialog?.contains(activeElement))
    && !(activeElement instanceof HTMLButtonElement && activeElement.disabled);
  if (!approvalNeedsFocus(requestId, focusedApprovalId, focusInsideDialog)) {
    return;
  }
  if (focusedApprovalId !== requestId) {
    expiredApprovalId = null;
  }
  focusedApprovalId = requestId;
  window.requestAnimationFrame(() => {
    if (snapshot?.pendingApproval?.requestId !== requestId) return;
    const reject = document.querySelector<HTMLButtonElement>("[data-reject-host-approval]");
    if (reject && !reject.disabled) {
      reject.focus();
    } else {
      document.querySelector<HTMLElement>(".approval-dialog")?.focus();
    }
  });
}

function handleApprovalKeyboard(event: KeyboardEvent): void {
  const dialog = document.querySelector<HTMLElement>(".approval-dialog");
  if (!dialog) {
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
  const nextIndex = approvalTabFocusIndex(
    controls.length,
    controls.indexOf(document.activeElement as HTMLElement),
    event.shiftKey,
  );
  if (nextIndex === null) {
    return;
  }
  event.preventDefault();
  (nextIndex < 0 ? dialog : controls[nextIndex])?.focus();
}

function renderSidebar(): string {
  const activeNavigationView = navigationViewFor(activeView as DeskLinkView);
  const sidebarItems = DESKTOP_NAV_ITEMS.filter(({ id }) => id !== "settings");
  return `
    <aside class="app-sidebar" aria-label="DeskLink 主导航">
      <div class="sidebar-brand">
        ${icon("monitor-check", "sidebar-brand-mark")}
        <div><strong>DeskLink</strong></div>
      </div>
      <nav class="sidebar-nav" aria-label="主要功能">
        ${sidebarItems.map(({ id, label }) => `
          <button class="sidebar-nav-item ${activeNavigationView === id ? "sidebar-nav-item--active" : ""}" type="button" data-view="${id}" aria-current="${activeNavigationView === id ? "page" : "false"}">
            ${icon(id === "controller" ? "monitor-up" : "monitor")}<span>${label}</span>
          </button>
        `).join("")}
      </nav>
      <div class="sidebar-bottom">
        <button class="sidebar-nav-item sidebar-nav-item--secondary ${activeNavigationView === "settings" ? "sidebar-nav-item--active" : ""}" type="button" data-view="settings" aria-label="设置与诊断" aria-current="${activeNavigationView === "settings" ? "page" : "false"}">
          ${icon("settings-2")}<span class="sidebar-settings-label">设置与诊断</span>
        </button>
      </div>
    </aside>
  `;
}

function renderWorkspaceTopbar(): string {
  return `
    <header class="workspace-topbar">
      <div class="workspace-context">
        ${snapshot ? renderHostStatusChip(snapshot) : ""}
      </div>
      <div class="workspace-topbar-actions">
        ${snapshot ? renderHostSharingActions(snapshot) : ""}
        ${account.user ? `<span class="account-session-email" title="当前登录账号">${escapeHtml(account.user.email)}</span><button class="topbar-account-button" type="button" data-account-logout ${accountBusy ? "disabled" : ""}>退出登录</button>` : ""}
        <button class="topbar-menu-toggle ${utilityMenuOpen ? "topbar-menu-toggle--active" : ""}" type="button" data-toggle-utility-menu aria-label="更多功能" aria-haspopup="menu" aria-expanded="${utilityMenuOpen}" aria-controls="utility-menu" title="更多功能">${icon("ellipsis")}<span>更多</span></button>
        ${renderUtilityMenu()}
      </div>
    </header>
  `;
}

function renderHostSharingActions(state: HostSnapshot): string {
  const paused = state.runtime.state === "paused";
  if (!state.connection && !paused) return "";
  return `<button class="button button--secondary button--compact" type="button" data-host-sharing="${paused ? "resume" : "pause"}" ${hostSharingBusy ? 'disabled aria-busy="true"' : ""} title="${paused ? "恢复本机共享，保留信任和固定密码" : "停止本机共享，保留信任；当前连接其他设备的会话继续"}">${hostSharingBusy ? "正在处理…" : paused ? "恢复共享" : "暂停共享"}</button>`;
}

function renderUtilityMenu(): string {
  const utilityItems: ReadonlyArray<{ id: DeskLinkView; label: string; icon: Parameters<typeof icon>[0] }> = [
    { id: "devices", label: "已批准设备", icon: "shield-check" },
  ];
  if (!utilityMenuOpen) {
    return "";
  }
  return `
    <div id="utility-menu" class="utility-menu" role="menu" aria-label="更多功能" aria-orientation="vertical">
      ${utilityItems.map(({ id, label, icon: iconName }) => `
        <button class="utility-menu-item ${activeView === id ? "utility-menu-item--active" : ""}" type="button" role="menuitem" data-view="${id}" aria-current="${activeView === id ? "page" : "false"}">
          ${icon(iconName)}<span>${label}</span>
        </button>
      `).join("")}
      <div class="utility-menu-divider" role="separator"></div>
      <button class="utility-menu-item" type="button" role="menuitem" data-view="about">${icon("circle-help")}<span>关于 DeskLink</span></button>
      <button class="utility-menu-item" type="button" role="menuitem" data-open-github>${icon("git-fork")}<span>项目主页</span></button>
      ${accountLocalMode && !account.user ? `<div class="utility-menu-divider" role="separator"></div><button class="utility-menu-item" type="button" role="menuitem" data-account-login>${icon("key-round")}<span>登录账号</span></button>` : ""}
    </div>
  `;
}

function renderHostStatusChip(state: HostSnapshot): string {
  const status = hostStatusSummary(state);
  const action = status.tone === "attention" ? "data-open-overview" : "data-open-connection";
  const destination = status.tone === "attention" ? "设置 / 诊断" : "共享此设备";
  return `<button class="host-status-chip host-status-chip--${status.tone}" type="button" ${action} aria-label="${escapeHtml(status.title)}，${escapeHtml(status.detail)}，打开${destination}">${escapeHtml(status.title)}</button>`;
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
    <div class="loading-layout" role="status" aria-live="polite">
      <p>正在加载…</p>
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
    </div>
  `;
}

function renderFatalState(): string {
  return `
    <div class="empty-state empty-state--error">
      ${icon("circle-alert", "empty-symbol")}
      <h1>无法读取 DeskLink 状态</h1>
      <p>正在重试，请稍候。</p>
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
    <div class="page-layout page-layout--form page-layout--share">
      <header class="page-heading">
        <div>
          <h1>共享此设备</h1>
        </div>
      </header>

      ${state.connectionError ? renderStateWarnings(state) : ""}

      <section class="share-device-card" aria-label="本机连接信息">
        ${state.connection
          ? `<div class="pairing-credential share-device-identity"><span>本机 ID</span><strong id="host-access-heading">${escapeHtml(state.deviceId ? formatDeviceId(state.deviceId) : "暂时无法读取")}</strong>${state.deviceId ? '<button class="button button--secondary button--compact" type="button" data-copy-host-id>复制 ID</button>' : ""}</div>
             <div class="page-actions share-device-actions">
               ${renderHostSharingActions(state)}
               ${renderPairingAction(state, "primary")}
               <button class="button button--secondary" type="button" data-open-fixed-access>${state.fixedPasswordEnabled ? "查看固定密码" : "设置固定密码"}</button>
             </div>`
          : `<button class="button button--primary" type="button" data-setup-managed ${managedSetupBusy ? 'disabled aria-busy="true"' : ""}>${managedSetupBusy ? "正在启用…" : "启用远程连接"}</button>`}
      </section>

      <details class="advanced-settings" data-connection-advanced ${connectionAdvancedOpen ? "open" : ""}>
        <summary>高级设置</summary>
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
          <small>自建中继的 IP 与 UDP 端口。</small>
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
          <small>须与中继证书名称一致。</small>
        </div>

        <div class="field--wide">
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
              ${state.connection?.hasSavedKey ? "<small>留空保留现有密钥。</small>" : ""}
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
            </div>
            <div class="advanced-settings-action">
              <button class="button button--secondary button--compact" type="button" data-generate-connection ${saving ? "disabled" : ""}>重新生成连接凭据</button>
              <small>重新生成后，已经配对的电脑需要再次配对。</small>
            </div>
          </div>
        </div>

        <div class="form-actions field--wide">
          <button class="button button--primary" type="submit" ${saving ? "disabled" : ""} ${saving ? 'aria-busy="true"' : ""}>
            ${saving ? "正在保存共享设置…" : "保存共享设置"}
          </button>
          <button class="button button--secondary" type="button" data-cancel-connection ${saving ? "disabled" : ""}>
            取消
          </button>
        </div>
      </form>
      </details>
    </div>
  `;
}

function renderDevices(state: HostSnapshot): string {
  return `
    <div class="page-layout">
      <header class="page-heading">
        <div>
          <h1>已批准设备</h1>
        </div>
        <div class="page-actions">
          ${renderPairingAction(state, "primary")}
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

    </div>
  `;
}

function renderDeviceEmptyState(): string {
  return `
    <div class="empty-state empty-state--devices">
      ${icon("monitor", "empty-device")}
      <h2>还没有已批准的电脑</h2>
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
  const expired = pairingSession !== null && pairingSession.expiresAtUnixS <= Math.floor(Date.now() / 1000);
  const session = expired ? null : pairingSession;
  const active = state.pairingActive && !expired;
  return `
    <div class="page-layout page-layout--pairing">
      <header class="page-heading page-heading--pairing">
        <div>
          <button class="back-button" type="button" data-open-connection aria-label="返回共享此设备">${icon("arrow-left")}共享此设备</button>
          <h1>临时连接</h1>
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
                <h2 id="pairing-credentials-heading">连接信息</h2>
                <strong data-pairing-countdown>${formatPairingRemaining(session.expiresAtUnixS)}</strong>
              </div>
              <div class="pairing-qr-layout">
                <div class="pairing-qr" aria-label="DeskLink 连接二维码">
                  ${session.qrSvg}
                </div>
                <div class="pairing-qr-copy">
                  <strong>扫码连接</strong>
                </div>
              </div>
              <div class="pairing-credentials">
                <div class="pairing-credential">
                  <span>设备 ID</span>
                  <strong id="pairing-device-id">${escapeHtml(formatDeviceId(session.deviceId))}</strong>
                  <button class="button button--secondary button--compact" type="button" data-copy-device-id ${pairingBusy ? "disabled" : ""}>复制设备 ID</button>
                </div>
                <div class="pairing-credential pairing-credential--password">
                  <span>临时密码</span>
                  <strong id="pairing-temporary-password">${escapeHtml(session.temporaryPassword)}</strong>
                  <button class="button button--secondary button--compact" type="button" data-copy-temporary-password ${pairingBusy ? "disabled" : ""}>复制临时密码</button>
                </div>
              </div>
              <div class="pairing-card-actions">
                <button class="button button--secondary" type="button" data-cancel-pairing ${pairingBusy ? "disabled" : ""}>${pairingBusy ? "正在结束…" : "结束本次连接窗口"}</button>
              </div>
              <p class="secret-note" id="pairing-secret-note">连接请求需在本机确认，请勿公开密码。</p>
            </section>
          `
          : `
            <section class="pairing-card pairing-card--unavailable">
              ${icon(active ? "loader-circle" : "circle-x", active ? "empty-symbol icon-spin" : "empty-symbol")}
              <h2>${active ? "本机正在等待连接" : "上次连接没有完成"}</h2>
              <p>${
                active
                  ? "请在生成密码的窗口查看，或结束等待后重新生成。"
                  : "请重新生成临时密码。"
              }</p>
              <div class="pairing-card-actions">
                ${active
                  ? `<button class="button button--secondary" type="button" data-cancel-pairing ${pairingBusy ? "disabled" : ""}>结束当前等待</button>`
                  : `<button class="button button--primary" type="button" data-start-pairing ${pairingBusy ? "disabled" : ""}>重新生成临时密码</button>`}
              </div>
            </section>
          `
      }

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
          <button class="back-button" type="button" data-open-connection aria-label="返回共享此设备">${icon("arrow-left")}共享此设备</button>
          <h1>固定访问密码</h1>
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
                  <h2 id="fixed-access-heading">连接信息</h2>
                </div>
                <div class="pairing-credentials">
                  <div class="pairing-credential">
                    <span>设备 ID</span>
                    <strong id="fixed-access-device-id">${escapeHtml(formatDeviceId(fixedAccess.deviceId))}</strong>
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
                        <strong>${fixedAccessConfirmation === "regenerate" ? "更换后旧密码立即失效，已批准设备不受影响。" : "关闭后将无法使用固定密码连接，已批准设备不受影响。"}</strong>
                        <button class="button button--secondary" type="button" data-cancel-fixed-access-action>保留当前密码</button>
                        <button class="button button--danger-quiet" type="button" data-confirm-fixed-access-action>${fixedAccessConfirmation === "regenerate" ? "更换并使旧密码失效" : "关闭固定密码"}</button>
                      </div>`
                    : `<button class="button button--secondary" type="button" data-regenerate-fixed-access ${fixedAccessBusy ? "disabled" : ""}>${fixedAccessBusy ? "正在更换…" : "更换固定密码"}</button>
                       <button class="button button--danger-quiet" type="button" data-disable-fixed-access ${fixedAccessBusy ? "disabled" : ""}>关闭固定密码</button>`}
                </div>
              </section>
            `
            : `<section class="pairing-card pairing-card--unavailable" role="status">${fixedAccessBusy ? "<h2>正在读取固定密码…</h2>" : '<button class="button button--primary" type="button" data-load-fixed-access>重试</button>'}</section>`
          : `
            <section class="pairing-card pairing-card--unavailable">
              ${icon("key-round", "empty-symbol")}
              <div class="pairing-card-actions">
                <button class="button button--primary" type="button" data-regenerate-fixed-access ${!canEnable || fixedAccessBusy ? "disabled" : ""}>${fixedAccessBusy ? "正在启用…" : "启用固定密码"}</button>
              </div>
            </section>
          `}

      <p class="secret-note">新设备首次连接需在本机确认，请勿公开密码。</p>
    </div>
  `;
}

function renderSettings(): string {
  return `
    <div class="page-layout page-layout--settings">
      <header class="page-heading">
        <div>
          <h1>设置 / 诊断</h1>
        </div>
      </header>

      ${preferencesLoading && !preferences
        ? `<div class="settings-loading" role="status">
            <p>正在读取设置…</p>
          </div>`
        : preferences
          ? renderPreferences(preferences)
          : `<section class="settings-unavailable">
              ${icon("circle-alert", "empty-symbol")}
              <h2>无法读取 Windows 设置</h2>
              <p>请重新打开设置重试。</p>
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
      </div>
      <div class="settings-row">
        <div class="settings-row-copy">
          <strong>登录 Windows 后自动启动</strong>
        </div>
        <label class="switch-control">
          <input type="checkbox" aria-label="登录 Windows 后自动启动" data-launch-at-login ${settings.launchAtLogin ? "checked" : ""} ${launchAtLoginBusy ? "disabled" : ""}>
          <span aria-hidden="true"></span>
          <b>${launchAtLoginBusy ? "正在保存" : settings.launchAtLogin ? "已开启" : "已关闭"}</b>
        </label>
      </div>
      <div class="settings-row">
        <div class="settings-row-copy">
          <strong>关闭窗口后继续运行</strong>
          <p>关闭窗口后仍可被连接。</p>
        </div>
        <span class="settings-value">${settings.closeToTray ? "系统托盘" : "直接退出"}</span>
      </div>
    </section>

    <section class="settings-group" aria-labelledby="diagnostics-sharing-heading">
      <div class="settings-group-heading">
        <h2 id="diagnostics-sharing-heading">连接问题诊断</h2>
      </div>
      <div class="settings-row settings-row--diagnostics">
        <div class="settings-row-copy">
          <strong>共享脱敏诊断</strong>
          <p>仅上传连接状态，保留 14 天；不含屏幕、按键、密码或密钥。</p>
        </div>
        <label class="switch-control">
          <input type="checkbox" aria-label="共享脱敏诊断" data-diagnostics-sharing ${settings.diagnosticsSharingEnabled ? "checked" : ""} ${diagnosticsSharingBusy ? "disabled" : ""}>
          <span aria-hidden="true"></span>
          <b>${diagnosticsSharingBusy ? "正在保存" : settings.diagnosticsSharingEnabled ? "已开启" : "已关闭"}</b>
        </label>
      </div>
      ${settings.diagnosticsSharingEnabled
        ? `<div class="settings-inline-action">
            <button class="button button--secondary button--compact" type="button" data-upload-diagnostics ${diagnosticsUploadBusy ? "disabled" : ""}>
              ${diagnosticsUploadBusy ? "正在发送…" : "立即发送诊断"}
            </button>
          </div>`
        : ""}
    </section>

    <section class="settings-group" aria-labelledby="update-settings-heading">
      <div class="settings-group-heading">
        <h2 id="update-settings-heading">应用更新</h2>
      </div>
      ${renderWindowsUpdate(settings.version)}
    </section>

    <section class="settings-exit" aria-labelledby="exit-settings-heading">
      <div>
        <h2 id="exit-settings-heading">退出</h2>
      </div>
      ${quitConfirming
        ? `<div class="settings-exit-confirm" role="group" aria-label="确认退出 DeskLink">
            <strong>退出后本机将离线，直到再次启动。</strong>
            <button class="button button--secondary" type="button" data-cancel-quit>继续在后台运行</button>
            <button class="button button--danger-quiet" type="button" data-confirm-quit>停止服务并退出</button>
          </div>`
        : '<button class="button button--secondary" type="button" data-request-quit>退出 DeskLink</button>'}
    </section>
  `;
}

function renderWindowsUpdate(currentVersion: string): string {
  return `<div data-windows-update-region="settings" data-update-current-version="${escapeHtml(currentVersion)}"></div>`;
}

function windowsUpdateMarkup(currentVersion: string, banner: boolean): string {
  const state = windowsUpdater.state;
  const button = (action: string, label: string, primary = false) =>
    `<button class="button button--${primary ? "primary" : "secondary"} button--compact" type="button" data-update-action="${action}">${label}</button>`;
  if (state.kind === "installing") {
    return `<div class="windows-update-content"><div class="windows-update-copy"><strong data-update-message role="status" aria-live="polite"></strong><progress data-update-progress aria-label="更新下载进度"></progress><p data-update-cancel-error role="alert" hidden></p></div>${button("cancel", "取消下载")}</div>`;
  }
  let title = `当前版本 DeskLink ${escapeHtml(currentVersion)}`;
  let action = button("check", "检查更新");
  let error = "";
  if (state.kind === "available") {
    title = `DeskLink ${escapeHtml(state.latestVersion)} 可更新`;
    action = button("install", "更新并重启", true);
  } else if (state.kind === "installError") {
    title = "更新失败";
    error = `<p class="windows-update-error" role="alert">${escapeHtml(state.message)}</p>`;
    action = button("install", "重试更新", true);
  } else if (state.kind === "checking") {
    title = "正在检查更新…";
    action = "";
  } else if (state.kind === "current") {
    title = "已是最新版本";
  } else if (state.kind === "unavailable") {
    title = "暂时没有可用更新";
  } else if (state.kind === "error") {
    title = "暂时无法检查更新";
    action = button("check", "重试");
  }
  return `<div class="windows-update-content"><div class="windows-update-copy"><strong role="status" aria-live="polite">${title}</strong>${error}</div><div class="windows-update-actions">${action}${banner ? button("dismiss", "稍后") : ""}</div></div>`;
}

function updateWindowsUpdateRegions(): void {
  windowsUpdateBanner.hidden = !windowsUpdater.bannerVisible;
  document.querySelectorAll<HTMLElement>("[data-windows-update-region]").forEach((region) => {
    if (region.hidden) return;
    const state = windowsUpdater.state;
    const banner = region.dataset.windowsUpdateRegion === "banner";
    const currentVersion = region.dataset.updateCurrentVersion || applicationVersion;
    const key = state.kind === "installing" ? "installing" : JSON.stringify([state, currentVersion]);
    const focused = document.activeElement;
    const focusInside = focused === region || (focused instanceof HTMLElement && region.contains(focused));
    const focusedAction = focused instanceof HTMLButtonElement && focusInside
      ? focused.dataset.updateAction : region.dataset.updateFocusAction;
    const restoreFocus = () => {
      if (!focusInside) return;
      if (focusedAction) region.dataset.updateFocusAction = focusedAction;
      const actions = Array.from(region.querySelectorAll<HTMLButtonElement>("[data-update-action]"))
        .filter((button) => !button.hidden && !button.disabled);
      const next = actions.find((button) => button.dataset.updateAction === focusedAction) ?? actions[0] ?? region;
      if (next === region) region.tabIndex = -1;
      if (document.activeElement !== next) next.focus({ preventScroll: true });
    };
    if (region.dataset.updateKey !== key) {
      region.innerHTML = windowsUpdateMarkup(currentVersion, banner);
      region.dataset.updateKey = key;
    }
    if (!windowsUpdateBoundRegions.has(region)) {
      region.addEventListener("click", (event) => {
        const button = event.target instanceof Element
          ? event.target.closest<HTMLButtonElement>("[data-update-action]") : null;
        if (!button || !region.contains(button) || button.disabled) return;
        if (button.dataset.updateAction === "install") void windowsUpdater.install();
        else if (button.dataset.updateAction === "cancel") void windowsUpdater.cancel();
        else if (button.dataset.updateAction === "dismiss") windowsUpdater.dismiss();
        else if (button.dataset.updateAction === "check") void checkForWindowsUpdate();
      });
      windowsUpdateBoundRegions.add(region);
    }
    if (state.kind !== "installing") {
      restoreFocus();
      return;
    }
    const message = region.querySelector<HTMLElement>("[data-update-message]");
    if (message) message.textContent = windowsUpdateProgressLabel(state.progress, state.cancelling);
    const progress = region.querySelector<HTMLProgressElement>("[data-update-progress]");
    if (progress) {
      progress.hidden = state.progress.stage !== "downloading";
      const total = state.progress.totalBytes;
      if (total && Number.isFinite(total) && total > 0) {
        progress.max = total;
        progress.value = Math.max(0, Math.min(total, state.progress.downloadedBytes ?? 0));
      } else {
        progress.removeAttribute("value");
      }
    }
    const cancel = region.querySelector<HTMLButtonElement>('[data-update-action="cancel"]');
    if (cancel) {
      cancel.hidden = state.progress.stage !== "downloading";
      cancel.disabled = state.cancelling;
    }
    const error = region.querySelector<HTMLElement>("[data-update-cancel-error]");
    if (error) {
      error.hidden = !state.cancelError;
      error.textContent = state.cancelError ?? "";
    }
    restoreFocus();
  });
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
  pairingSession.qrSvg = "";
}

function clearFixedAccessSecrets(): void {
  fixedAccessRequests.begin();
  if (fixedAccess) {
    fixedAccess.password = "";
  }
  fixedAccess = null;
  fixedAccessConfirmation = null;
}

async function loadFixedAccess(): Promise<void> {
  if (activeView !== "fixedAccess" || !snapshot?.fixedPasswordEnabled || fixedAccessBusy || fixedAccess) {
    return;
  }
  fixedAccessBusy = true;
  const request = fixedAccessRequests.begin();
  let reloadOnReturn = false;
  render();
  try {
    const nextAccess = await getFixedAccessPassword();
    if (!fixedAccessRequests.isCurrent(request) || activeView !== "fixedAccess" || accountBusy
      || !canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode)) {
      nextAccess.password = "";
      reloadOnReturn = true;
      return;
    }
    fixedAccess = nextAccess;
  } catch (error) {
    if (fixedAccessRequests.isCurrent(request) && activeView === "fixedAccess") {
      clearFixedAccessSecrets();
      feedback = { tone: "error", message: normalizeError(error) };
    }
  } finally {
    fixedAccessBusy = false;
    if (activeView === "fixedAccess") {
      render();
      if (reloadOnReturn && snapshot?.fixedPasswordEnabled && !accountBusy
        && canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode)) void loadFixedAccess();
    }
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
  const request = fixedAccessRequests.begin();
  let reloadOnReturn = false;
  render();
  try {
    const nextAccess = await regenerateFixedAccessPassword();
    if (fixedAccessRequests.isCurrent(request) && activeView === "fixedAccess" && !accountBusy
      && canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode)) {
      fixedAccess = nextAccess;
      feedback = { tone: "success", message: "固定密码已更新。" };
    } else {
      nextAccess.password = "";
      reloadOnReturn = true;
    }
    if (canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode)) {
      snapshotRequests.begin();
      if (snapshot) {
        snapshot.fixedPasswordEnabled = true;
        snapshot.fixedPasswordError = null;
      }
      await refreshSnapshot(false);
    }
  } catch (error) {
    if (fixedAccessRequests.isCurrent(request) && activeView === "fixedAccess") {
      feedback = { tone: "error", message: normalizeError(error) };
    }
  } finally {
    fixedAccessBusy = false;
    if (activeView === "fixedAccess") {
      render();
      if (reloadOnReturn && snapshot?.fixedPasswordEnabled && !accountBusy
        && canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode)) void loadFixedAccess();
    }
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
    const nextSnapshot = await disableFixedAccessPassword();
    if (!canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode) || accountBusy) return;
    snapshotRequests.begin();
    if (snapshot) {
      snapshot.fixedPasswordEnabled = nextSnapshot.fixedPasswordEnabled;
      snapshot.fixedPasswordError = nextSnapshot.fixedPasswordError;
    }
    feedback = { tone: "success", message: "固定密码已关闭，旧密码已经失效。" };
    await refreshSnapshot(false);
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

function checkForWindowsUpdate(force = true): Promise<void> {
  return windowsUpdater.check(force);
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

function bindInteractions(preservedController = false): void {
  document.querySelectorAll<HTMLButtonElement>("[data-host-sharing]").forEach((button) => {
    button.addEventListener("click", () => { void changeHostSharing(button.dataset.hostSharing === "resume"); });
  });
  document.querySelector<HTMLButtonElement>("[data-account-logout]")?.addEventListener("click", () => void logoutAccount());
  document.querySelector<HTMLButtonElement>("[data-account-login]")?.addEventListener("click", () => enterAccountLogin());
  document.querySelectorAll<HTMLButtonElement>("[data-open-github]").forEach((button) => {
    button.addEventListener("click", () => void openGithub());
  });
  document.querySelector<HTMLButtonElement>("[data-toggle-utility-menu]")?.addEventListener("click", () => {
    utilityMenuOpen = !utilityMenuOpen;
    render();
    if (utilityMenuOpen) {
      window.requestAnimationFrame(() => {
        document.querySelector<HTMLButtonElement>('.utility-menu [role="menuitem"]')?.focus();
      });
    } else {
      window.requestAnimationFrame(() => {
        document.querySelector<HTMLButtonElement>("[data-toggle-utility-menu]")?.focus();
      });
    }
  });
  document.querySelector<HTMLElement>(".utility-menu")?.addEventListener("keydown", handleUtilityMenuKeyboard);
  if (activeView === "controller" && !preservedController) {
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
  navigationButtons.forEach((button) => {
    button.addEventListener("click", () => {
      if (activeView === "fixedAccess") {
        clearFixedAccessSecrets();
      }
      fixedAccessConfirmation = null;
      activeView = button.dataset.view as View;
      utilityMenuOpen = false;
      quitConfirming = false;
      if (activeView === "connection") {
        connectionDraft = null;
        connectionAdvancedOpen = false;
      }
      feedback = null;
      render();
      if (button.closest(".utility-menu")) {
        window.requestAnimationFrame(() => {
          document.querySelector<HTMLButtonElement>("[data-toggle-utility-menu]")?.focus();
        });
      }
      if (activeView === "fixedAccess") {
        void loadFixedAccess();
      } else if (activeView === "settings" && !preferences) {
        void loadPreferences();
      }
      if (activeView === "settings") {
        void checkForWindowsUpdate(false);
      }
    });
  });
  // Keep the sidebar's arrow navigation separate from utility menu and inline
  // actions, which have their own keyboard interaction.
  const primaryNavigationButtons = Array.from(
    document.querySelectorAll<HTMLButtonElement>(".sidebar-nav [data-view]"),
  );
  primaryNavigationButtons.forEach((button, currentIndex) => {
    button.addEventListener("keydown", (event) => {
      const nextIndex = nextMenuIndex(currentIndex, primaryNavigationButtons.length, event.key)
        ?? nextTabIndex(currentIndex, primaryNavigationButtons.length, event.key);
      if (nextIndex === null) {
        return;
      }
      event.preventDefault();
      primaryNavigationButtons[nextIndex]?.focus();
      primaryNavigationButtons[nextIndex]?.click();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-setup-managed]").forEach((button) => {
    button.addEventListener("click", () => void enableManagedConnection());
  });
  document.querySelector<HTMLButtonElement>("[data-restart-host]")?.addEventListener("click", () => {
    void restartStoppedHost();
  });
  document.querySelector<HTMLButtonElement>("[data-export-diagnostics]")?.addEventListener("click", () => {
    void exportDiagnostics();
  });
  document.querySelectorAll<HTMLButtonElement>("[data-open-connection]").forEach((button) => {
    button.addEventListener("click", () => {
      clearFixedAccessSecrets();
      activeView = "connection";
      utilityMenuOpen = false;
      connectionDraft = null;
      connectionAdvancedOpen = false;
      feedback = null;
      render();
    });
  });
  document.querySelector<HTMLButtonElement>("[data-open-overview]")?.addEventListener("click", () => {
    clearFixedAccessSecrets();
    activeView = "settings";
    utilityMenuOpen = false;
    feedback = null;
    if (!preferences) {
      void loadPreferences();
    }
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-open-devices]")?.addEventListener("click", () => {
    clearFixedAccessSecrets();
    activeView = "devices";
    utilityMenuOpen = false;
    feedback = null;
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-open-fixed-access]")?.addEventListener("click", () => {
    activeView = "fixedAccess";
    utilityMenuOpen = false;
    feedback = null;
    clearFixedAccessSecrets();
    render();
    void loadFixedAccess();
  });
  document.querySelector<HTMLButtonElement>("[data-load-fixed-access]")?.addEventListener("click", () => {
    void loadFixedAccess();
  });
  document.querySelector<HTMLButtonElement>("[data-open-controller]")?.addEventListener("click", () => {
    clearFixedAccessSecrets();
    activeView = "controller";
    utilityMenuOpen = false;
    feedback = null;
    render();
  });
  document.querySelectorAll<HTMLButtonElement>("[data-start-pairing]").forEach((button) => {
    button.addEventListener("click", () => void beginPairing());
  });
  document.querySelectorAll<HTMLButtonElement>("[data-open-pairing]").forEach((button) => {
    button.addEventListener("click", () => {
      activeView = "pairing";
      utilityMenuOpen = false;
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
    activeView = "connection";
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

function enterAccountLogin(): void {
  clearFixedAccessSecrets();
  accountLocalMode = false;
  setAccountLocalModeEnabled(false);
  accountError = null;
  accountNotice = null;
  accountMode = "login";
  utilityMenuOpen = false;
  render();
}

async function openGithub(): Promise<void> {
  try {
    await openGithubRepository();
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
  let hostPresentationChanged = false;
  if (showLoading) {
    loading = true;
    feedback = null;
    render();
  }
  try {
    const nextSnapshot = await getHostSnapshot();
    if (!snapshotRequests.isCurrent(request)) {
      return;
    }
    hostPresentationChanged = hostSnapshotPresentationChanged(snapshot, nextSnapshot);
    snapshot = nextSnapshot;
    if (snapshotReadFeedback && feedback === snapshotReadFeedback) {
      feedback = null;
      hostPresentationChanged = true;
    }
    snapshotReadFeedback = null;
    if (snapshotRecoveryTimer !== null) {
      window.clearTimeout(snapshotRecoveryTimer);
      snapshotRecoveryTimer = null;
    }
    if (pairingSession && snapshot.runtime.state === "connected") {
      clearPairingSecrets();
      pairingSession = null;
      if (activeView === "pairing") {
        activeView = "controller";
      }
      feedback = { tone: "success", message: "已连接。" };
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
      hostPresentationChanged = snapshot !== null;
      snapshot = null;
      scheduleSnapshotRecovery();
    }
    const nextFeedback: NonNullable<Feedback> = {
      tone: "error",
      message: showLoading
        ? normalizeError(error)
        : `无法读取最新状态，界面暂时保留上次结果。${normalizeError(error)}`,
    };
    hostPresentationChanged ||= feedback?.tone !== nextFeedback.tone || feedback?.message !== nextFeedback.message;
    feedback = nextFeedback;
    snapshotReadFeedback = nextFeedback;
  } finally {
    if (!snapshotRequests.isCurrent(request)) {
      return;
    }
    hostPresentationChanged ||= loading;
    loading = false;
    if (showLoading || hostPresentationChanged) {
      render();
    }
  }
}

function scheduleSnapshotRecovery(): void {
  if (snapshotRecoveryTimer !== null || !canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode)) {
    return;
  }
  snapshotRecoveryTimer = window.setTimeout(() => {
    snapshotRecoveryTimer = null;
    void refreshSnapshot();
  }, 1800);
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

async function changeHostSharing(resume: boolean): Promise<void> {
  if (hostSharingBusy) return;
  hostSharingBusy = true;
  render();
  try {
    await (resume ? resumeHostSharing() : pauseHostSharing());
    clearPairingSecrets();
    pairingSession = null;
    feedback = { tone: "info", message: resume ? "本机共享已恢复，已保留信任与固定密码。" : "本机共享已暂停，信任与固定密码已保留。" };
  } catch (error) {
    feedback = { tone: "error", message: normalizeError(error) };
  } finally {
    await refreshSnapshot(false);
    hostSharingBusy = false;
    render();
  }
}

async function enableManagedConnection(): Promise<void> {
  if (managedSetupBusy || snapshot?.connection) {
    return;
  }
  managedSetupBusy = true;
  feedback = null;
  render();
  try {
    snapshot = await setupManagedConnection();
    connectionDraft = null;
    connectionAdvancedOpen = false;
    feedback = null;
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
  const originView = activeView;
  clearPairingSecrets();
  pairingSession = null;
  feedback = null;
  render();
  try {
    const nextSession = await startPairingSession();
    if (!canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode) || accountBusy) {
      nextSession.temporaryPassword = "";
      nextSession.qrSvg = "";
      return;
    }
    snapshotRequests.begin();
    clearPairingSecrets();
    pairingSession = nextSession;
    if (snapshot) snapshot.pairingActive = true;
    if (activeView === originView) activeView = "pairing";
    await refreshSnapshot(false);
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
    await cancelPairingSession();
    if (!canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode) || accountBusy) return;
    snapshotRequests.begin();
    if (snapshot) snapshot.pairingActive = false;
    if (activeView === "pairing") activeView = "connection";
    feedback = { tone: "success", message: "临时密码已清除，本机在线服务已恢复。" };
    await refreshSnapshot(false);
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
      if (!source) {
        throw new Error("此信息已不可用。");
      }
      const focused = document.activeElement;
      const copyInput = document.createElement("textarea");
      copyInput.value = text;
      copyInput.readOnly = true;
      copyInput.tabIndex = -1;
      copyInput.setAttribute("aria-hidden", "true");
      copyInput.style.position = "fixed";
      copyInput.style.left = "-9999px";
      copyInput.style.top = "0";
      document.body.append(copyInput);
      try {
        copyInput.select();
        if (!document.execCommand("copy")) {
          throw new Error("Windows 未允许 DeskLink 复制此信息。");
        }
      } finally {
        copyInput.value = "";
        copyInput.remove();
        if (focused instanceof HTMLElement && focused.isConnected) {
          focused.focus({ preventScroll: true });
        }
      }
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
    feedback = { tone: "success", message: "共享设置已保存。" };
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

async function bootstrap(): Promise<void> {
  startWindowsUpdateChecks();
  const operation = accountOperations.begin("restore");
  try {
    const result = await accountRestore(operation.generation);
    if (!accountOperations.current(operation)) return;
    if (result.signedIn) await accountAcceptOperation(operation.generation);
    if (!accountOperations.current(operation)) return;
    await accountFinishOperation(operation.generation);
    if (!accountOperations.current(operation)) return;
    account = result;
    accountLocalMode = shouldStartInLocalMode(account.signedIn);
  } catch (error) {
    accountError = normalizeError(error);
    accountLocalMode = shouldStartInLocalMode(account.signedIn);
  } finally {
    accountLoading = false;
    render();
  }
  if (!account.signedIn && !accountLocalMode) return;
  await startWorkspace();
}

function startWindowsUpdateChecks(): void {
  void checkForWindowsUpdate(false);
  const recover = () => {
    if (document.visibilityState === "visible") void checkForWindowsUpdate(false);
  };
  window.addEventListener("focus", recover);
  window.addEventListener("online", recover);
  document.addEventListener("visibilitychange", recover);
}

function ensureWorkspaceHostListeners(): Promise<void> {
  const attempts = workspaceHostEvents.map((eventName) => {
    if (workspaceHostListeners.has(eventName)) return Promise.resolve();
    const pending = workspaceHostListenerAttempts.get(eventName);
    if (pending) return pending;
    const attempt = (async () => {
      await listen(eventName, () => {
        if (canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode)) {
          void refreshSnapshot(false);
        }
      });
      workspaceHostListeners.add(eventName);
    })().finally(() => workspaceHostListenerAttempts.delete(eventName));
    workspaceHostListenerAttempts.set(eventName, attempt);
    return attempt;
  });
  return Promise.allSettled(attempts).then(() => {});
}

function bindWorkspaceSnapshotRecovery(): void {
  if (workspaceRecoveryBound) return;
  const recover = () => {
    if (document.visibilityState !== "visible"
      || !canRecoverWorkspaceSnapshot(account.signedIn, accountLocalMode)) return;
    void ensureWorkspaceHostListeners();
    void refreshSnapshot(false);
  };
  window.addEventListener("focus", recover);
  document.addEventListener("visibilitychange", recover);
  window.setInterval(recover, 15_000);
  workspaceRecoveryBound = true;
}

async function startWorkspace(): Promise<void> {
  bindWorkspaceSnapshotRecovery();
  let listenerWaitTimer: number | undefined;
  try {
    await Promise.race([
      ensureWorkspaceHostListeners(),
      new Promise<void>((resolve) => {
        listenerWaitTimer = window.setTimeout(resolve, 3_000);
      }),
    ]);
  } finally {
    window.clearTimeout(listenerWaitTimer);
  }
  await initializeController(scheduleRender);
  await refreshSnapshot();
}

function updateCredentialExpiry(): void {
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
      focusNewApproval();
      feedback = { tone: "info", message: "远程控制请求已过期，对方需要重新发起连接。" };
      void refreshSnapshot(false);
    }
  }
  if (pairingSession) {
    const countdown = document.querySelector<HTMLElement>("[data-pairing-countdown]");
    if (countdown) {
      countdown.textContent = formatPairingRemaining(pairingSession.expiresAtUnixS);
    }
    if (pairingSession.expiresAtUnixS <= nowUnixS) {
      clearPairingSecrets();
      pairingSession = null;
      if (activeView === "pairing") {
        feedback = { tone: "info", message: "临时密码已过期，本次连接没有完成。请重新生成密码后再试。" };
        render();
      }
      void refreshSnapshot(false);
    }
  }
}

void bootstrap();
window.setInterval(updateCredentialExpiry, 1000);
