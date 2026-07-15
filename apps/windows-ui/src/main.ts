import "./styles.css";

import { listen } from "@tauri-apps/api/event";

import {
  cancelPairingSession,
  getHostSnapshot,
  revokeTrustedController,
  saveConnectionSettings,
  startPairingSession,
} from "./api";
import type {
  ConnectionSettingsInput,
  ConnectionSummary,
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
import { pairingCodeWithRelayAddress, parsePairingCode } from "./pairing-code";

type View = "overview" | "controller" | "connection" | "devices" | "pairing";
type Feedback = { tone: "success" | "error" | "info"; message: string } | null;

const applicationRoot = document.querySelector<HTMLElement>("#app");
if (!applicationRoot) {
  throw new Error("未找到 DeskLink 应用界面根节点");
}
const app: HTMLElement = applicationRoot;

let snapshot: HostSnapshot | null = null;
let activeView: View = "overview";
let loading = true;
let saving = false;
let pairingBusy = false;
let pairingSession: PairingSessionSummary | null = null;
let pairingRelayAddress: string | null = null;
let revokingFingerprint: string | null = null;
let feedback: Feedback = null;
let connectionDraft: ConnectionSettingsInput | null = null;

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function render(): void {
  prepareControllerRender();
  app.innerHTML = `
    <div class="app-shell">
      ${renderHeader()}
      ${renderNavigation()}
      <section class="workspace" aria-busy="${loading}">
        ${feedback ? renderFeedback(feedback) : ""}
        ${loading ? renderLoading() : renderCurrentView()}
      </section>
    </div>
  `;
  bindInteractions();
}

function renderHeader(): string {
  const protectionCopy = snapshot?.connectionError
    ? "需要检查本地保护"
    : "Windows 保护已启用";
  const protectionTone = snapshot?.connectionError ? "attention" : "secure";
  return `
    <header class="topbar">
      <div class="product-lockup" aria-label="DeskLink Windows 远程桌面">
        <span class="product-mark" aria-hidden="true"><span></span></span>
        <div>
          <strong>DeskLink</strong>
          <span>Windows 远程桌面</span>
        </div>
      </div>
      <div class="protection-state protection-state--${protectionTone}">
        <span class="protection-glyph" aria-hidden="true"></span>
        ${protectionCopy}
      </div>
    </header>
  `;
}

function renderNavigation(): string {
  const activeNavigationView = activeView === "pairing" ? "devices" : activeView;
  const items: Array<{ id: View; label: string }> = [
    { id: "overview", label: "概览" },
    { id: "controller", label: "控制电脑" },
    { id: "connection", label: "连接设置" },
    { id: "devices", label: "可信设备" },
  ];
  return `
    <nav class="section-nav" aria-label="DeskLink 功能导航">
      ${items
        .map(
          ({ id, label }) => `
            <button
              class="nav-item ${activeNavigationView === id ? "nav-item--active" : ""}"
              type="button"
              data-view="${id}"
              ${activeNavigationView === id ? 'aria-current="page"' : ""}
            >${label}</button>
          `,
        )
        .join("")}
    </nav>
  `;
}

function renderFeedback(item: NonNullable<Feedback>): string {
  return `
    <div class="feedback feedback--${item.tone}" role="status">
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
  const metrics = [
    {
      label: "中继服务器",
      value: connection?.relayAddress ?? "未配置",
      detail: connection?.serverName ?? "请添加连接设置",
    },
    {
      label: "会话",
      value: connection ? compactIdentifier(connection.sessionId) : "不可用",
      detail: connection ? "已为当前账户加密保护" : "没有已保存的会话",
    },
    {
      label: "可信设备",
      value: String(state.trustedControllers.length),
      detail: "已批准的控制端",
    },
  ];
  return `
    <div class="overview-stack">
      <section class="status-panel status-panel--${state.readiness}" aria-labelledby="status-heading">
        <div class="status-copy">
          <div class="status-label">
            <span class="status-light" aria-hidden="true"></span>
            主机状态
          </div>
          <h1 id="status-heading">${escapeHtml(state.title)}</h1>
          <p>${escapeHtml(state.detail)}</p>
        </div>
        <div class="status-actions">
          <button class="button button--primary" type="button" data-open-connection>
            ${connection ? "编辑连接" : "设置连接"}
          </button>
          ${renderPairingAction(state, "secondary")}
          <button class="button button--secondary" type="button" data-refresh>刷新状态</button>
        </div>
      </section>

      ${renderStateWarnings(state)}

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

      <section class="recent-access" aria-labelledby="recent-access-heading">
        <div class="section-heading">
          <div>
            <h2 id="recent-access-heading">可信访问</h2>
            <p>只有在这台 Windows 设备上批准过的控制端才能重新连接。</p>
          </div>
          <div class="section-actions">
            ${renderPairingAction(state, "text")}
            <button class="text-button" type="button" data-open-devices>查看设备</button>
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

function renderRelayDiagnostics(state: HostSnapshot): string {
  const relay = state.relayStatus;
  if (relay.mode === "unconfigured") {
    return "";
  }
  const stateLabel =
    relay.state === "ready"
      ? "可连接"
      : relay.state === "starting"
        ? "启动中"
        : relay.state === "offline"
          ? "网络未连接"
          : relay.state === "failed"
            ? "需要处理"
            : "未运行";
  const addresses = relay.addresses
    .map(
      (address) => `
        <li>
          <div>
            <code>${escapeHtml(address.relayAddress)}</code>
            <span title="${escapeHtml(address.interfaceName)}">${escapeHtml(address.interfaceName)}</span>
          </div>
          <small>${address.isPrimary ? "推荐地址" : "配对时可选择"}</small>
        </li>
      `,
    )
    .join("");
  return `
    <section class="relay-diagnostics" aria-labelledby="relay-diagnostics-heading">
      <div class="section-heading">
        <div>
          <h2 id="relay-diagnostics-heading">${relay.mode === "lan" ? "局域网连接检查" : "中继连接方式"}</h2>
          <p>${escapeHtml(relay.detail)}</p>
        </div>
        <span class="relay-health relay-health--${relay.state}">
          <span aria-hidden="true"></span>${stateLabel}
        </span>
      </div>
      ${addresses ? `<ul class="relay-address-list" aria-label="本机可用局域网地址">${addresses}</ul>` : ""}
      ${
        relay.mode === "lan" && relay.addresses.length > 1
          ? '<p class="relay-advice">检测到多个网卡。创建连接码后，请选择与另一台电脑处于同一 Wi-Fi 或有线网络的地址，通常不要选择 VPN 或虚拟网卡。</p>'
          : ""
      }
      ${
        relay.mode === "lan"
          ? '<p class="relay-advice">另一台电脑仍需允许 Windows 防火墙“专用网络”访问；部分访客 Wi-Fi 会阻止设备互相连接。</p>'
          : ""
      }
    </section>
  `;
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
  const fields = connectionDraft ?? connectionToInput(state.connection);
  return `
    <div class="page-layout page-layout--form">
      <header class="page-heading">
        <div>
          <h1>连接设置</h1>
          <p>这些信息用于将本机连接到另一台 DeskLink 设备使用的中继服务器。</p>
        </div>
        <div class="page-actions">
          <span class="storage-note">由 Windows DPAPI 加密保护</span>
          <button class="button button--secondary button--compact" type="button" data-generate-connection ${saving ? "disabled" : ""}>生成安全凭据</button>
        </div>
      </header>

      ${state.connectionError ? renderStateWarnings(state) : ""}

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
          <small>DeskLink 中继服务器的 IP 地址和端口。</small>
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
          <small>必须与中继服务器 TLS 证书中的名称一致。</small>
        </div>

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
          <small>用于识别此私有中继会话，它不是中继密钥。</small>
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
          <small>已保存的密钥不会重新返回此界面。</small>
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
          <small>初始值为 1，每次安全重连后递增。</small>
        </div>

        <div class="form-actions">
          <button class="button button--primary" type="submit" ${saving ? "disabled" : ""}>
            ${saving ? "正在保存连接…" : "保存连接"}
          </button>
          <button class="button button--secondary" type="button" data-cancel-connection ${saving ? "disabled" : ""}>
            取消修改
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
          <h1>可信设备</h1>
          <p>这里的控制端均已完成加密身份验证和本地批准。</p>
        </div>
        <div class="page-actions">
          ${renderPairingAction(state, "primary")}
          <button class="button button--secondary" type="button" data-refresh>刷新设备</button>
        </div>
      </header>

      ${state.trustedError ? renderStateWarnings(state) : ""}

      <section class="device-register" aria-label="可信控制端身份列表">
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
      <h2>还没有可信控制端</h2>
      <p>创建一个短期邀请，然后在本机核对并批准控制端身份。</p>
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
          <h2>控制端 ${escapeHtml(compactIdentifier(device.deviceId))}</h2>
          <p>批准于 ${formatDate(device.approvedAtUnixS)}</p>
        </div>
        <div class="device-actions">
          <span class="trusted-badge">已信任</span>
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
  const lanUnavailable =
    state.relayStatus.mode === "lan" &&
    (state.relayStatus.state === "failed" || state.relayStatus.state === "offline");
  const disabled =
    pairingBusy ||
    (!active && (!state.connection || Boolean(state.trustedError) || lanUnavailable));
  const className = presentation === "text" ? "text-button" : `button button--${presentation}`;
  const action = active ? "data-open-pairing" : "data-start-pairing";
  const label = active ? "查看当前配对" : pairingBusy ? "正在启动配对…" : "配对设备";
  const title = !state.connection
    ? 'title="请先保存连接设置，再开始配对"'
    : state.trustedError
      ? 'title="可信设备存储可用后才能开始配对"'
      : lanUnavailable
        ? 'title="连接局域网并让本机中继就绪后再开始配对"'
      : "";
  return `<button class="${className}" type="button" ${action} ${disabled ? "disabled" : ""} ${title}>${label}</button>`;
}

function renderPairing(state: HostSnapshot): string {
  const session = pairingSession;
  const active = state.pairingActive;
  const selectedRelayAddress = session ? currentPairingRelayAddress(state, session) : null;
  const displayedInvitation = session
    ? currentPairingInvitation(state, session)
    : "";
  return `
    <div class="page-layout page-layout--pairing">
      <header class="page-heading page-heading--pairing">
        <div>
          <button class="back-button" type="button" data-open-devices aria-label="返回可信设备">← 可信设备</button>
          <h1>配对控制端</h1>
          <p>邀请仅在短时间内有效，控制端仍需在本机获得 Windows 批准后才会被信任。</p>
        </div>
        <span class="pairing-state ${active ? "pairing-state--active" : ""}">
          <span aria-hidden="true"></span>${active ? "邀请有效" : "邀请已失效"}
        </span>
      </header>

      ${
        session
          ? `
            <section class="pairing-card" aria-labelledby="pairing-invitation-heading">
              <div class="pairing-card-heading">
                <div>
                  <span class="eyebrow">一次性连接码</span>
                  <h2 id="pairing-invitation-heading">发送给另一台 DeskLink 控制端</h2>
                </div>
                <strong data-pairing-countdown>${formatPairingRemaining(session.expiresAtUnixS)}</strong>
              </div>
              <label class="sr-only" for="pairing-invitation">配对连接码</label>
              ${
                state.relayStatus.mode === "lan" && state.relayStatus.addresses.length > 0
                  ? `
                    <label class="pairing-network" for="pairing-network-address">
                      <span>另一台电脑所在的网络</span>
                      <select id="pairing-network-address" data-pairing-address ${pairingBusy ? "disabled" : ""}>
                        ${state.relayStatus.addresses
                          .map(
                            (address) => `
                              <option value="${escapeHtml(address.relayAddress)}" ${address.relayAddress === selectedRelayAddress ? "selected" : ""}>
                                ${escapeHtml(address.relayAddress)} · ${escapeHtml(address.interfaceName)}${address.isPrimary ? "（推荐）" : ""}
                              </option>
                            `,
                          )
                          .join("")}
                      </select>
                      <small>如果装有 VPN 或虚拟网卡，请选择与另一台电脑处于同一局域网的地址。</small>
                    </label>
                  `
                  : ""
              }
              <textarea
                id="pairing-invitation"
                class="pairing-invitation"
                readonly
                spellcheck="false"
                aria-describedby="pairing-secret-note"
              >${escapeHtml(displayedInvitation)}</textarea>
              <div class="pairing-card-actions">
                <button class="button button--primary" type="button" data-copy-pairing ${pairingBusy ? "disabled" : ""}>复制连接码</button>
                <button class="button button--secondary" type="button" data-cancel-pairing ${pairingBusy ? "disabled" : ""}>${pairingBusy ? "正在恢复主机…" : "取消配对"}</button>
              </div>
              <p class="secret-note" id="pairing-secret-note">连接码已包含主机的中继地址和一次性邀请。请只发送给自己的 DeskLink 设备；局域网首次连接时，请允许 Windows 防火墙的专用网络访问。</p>
            </section>
          `
          : `
            <section class="pairing-card pairing-card--unavailable">
              <span class="empty-symbol" aria-hidden="true">${active ? "…" : "×"}</span>
              <h2>${active ? "本机正在等待配对" : "此邀请已失效"}</h2>
              <p>${
                active
                  ? "为保护连接安全，邀请只会显示在创建它的窗口中。如需重新获取，请取消本次配对并创建新邀请。"
                  : "邀请已过期或配对主机已停止，普通主机模式会自动恢复。"
              }</p>
              <div class="pairing-card-actions">
                <button class="button button--primary" type="button" data-cancel-pairing ${pairingBusy ? "disabled" : ""}>恢复普通主机模式</button>
              </div>
            </section>
          `
      }

      <div class="security-note security-note--pairing">
        <span class="security-note-mark" aria-hidden="true"></span>
        <div>
          <strong>批准操作不能远程完成</strong>
          <p>控制端连接后，Windows 会显示完整的设备 ID 和公钥指纹，并默认选择更安全的“否”。</p>
        </div>
      </div>
    </div>
  `;
}

function currentPairingRelayAddress(
  state: HostSnapshot,
  session: PairingSessionSummary,
): string | null {
  const available = state.relayStatus.addresses.map((address) => address.relayAddress);
  if (pairingRelayAddress && available.includes(pairingRelayAddress)) {
    return pairingRelayAddress;
  }
  const packaged = parsePairingCode(session.invitation, "", "")?.relayAddress;
  if (packaged && available.includes(packaged)) {
    return packaged;
  }
  return available[0] ?? packaged ?? null;
}

function currentPairingInvitation(
  state: HostSnapshot,
  session: PairingSessionSummary,
): string {
  const relayAddress = currentPairingRelayAddress(state, session);
  return relayAddress
    ? pairingCodeWithRelayAddress(session.invitation, relayAddress) ?? session.invitation
    : session.invitation;
}

function connectionToInput(connection: ConnectionSummary | null): ConnectionSettingsInput {
  return {
    relayAddress: connection?.relayAddress ?? "127.0.0.1:4433",
    serverName: connection?.serverName ?? "localhost",
    sessionId: connection?.sessionId ?? "",
    relayKey: "",
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

function bindInteractions(): void {
  if (activeView === "controller") {
    bindControllerInteractions();
  }
  document.querySelectorAll<HTMLButtonElement>("[data-view]").forEach((button) => {
    button.addEventListener("click", () => {
      activeView = button.dataset.view as View;
      feedback = null;
      render();
    });
  });
  document.querySelectorAll<HTMLButtonElement>("[data-refresh]").forEach((button) => {
    button.addEventListener("click", () => void refreshSnapshot());
  });
  document.querySelector<HTMLButtonElement>("[data-open-connection]")?.addEventListener("click", () => {
    activeView = "connection";
    connectionDraft = null;
    feedback = null;
    render();
  });
  document.querySelector<HTMLButtonElement>("[data-open-devices]")?.addEventListener("click", () => {
    activeView = "devices";
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
  document.querySelector<HTMLSelectElement>("[data-pairing-address]")?.addEventListener("change", (event) => {
    const selected = (event.currentTarget as HTMLSelectElement).value;
    if (snapshot?.relayStatus.addresses.some((address) => address.relayAddress === selected)) {
      pairingRelayAddress = selected;
      feedback = { tone: "info", message: "连接码已切换到所选局域网地址，请重新复制后发送到另一台电脑。" };
      render();
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
  document.querySelector<HTMLButtonElement>("[data-cancel-connection]")?.addEventListener("click", () => {
    connectionDraft = null;
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
    feedback = { tone: "info", message: "新的会话凭据已生成，请保存连接，让 Windows 对它们进行加密保护。" };
    render();
  });
  document
    .querySelector<HTMLFormElement>("[data-connection-form]")
    ?.addEventListener("submit", (event) => void submitConnection(event));
}

async function refreshSnapshot(showLoading = true): Promise<void> {
  loading = showLoading;
  if (showLoading) {
    feedback = null;
    render();
  }
  try {
    snapshot = await getHostSnapshot();
    if (!snapshot.pairingActive && pairingSession) {
      pairingSession.invitation = "";
      pairingSession = null;
      pairingRelayAddress = null;
    }
  } catch (error) {
    snapshot = null;
    feedback = {
      tone: "error",
      message: normalizeError(error),
    };
  } finally {
    loading = false;
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
    pairingRelayAddress = currentPairingRelayAddress(snapshot, pairingSession);
    snapshot.pairingActive = true;
    activeView = "pairing";
  } catch (error) {
    pairingSession = null;
    pairingRelayAddress = null;
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
    pairingSession.invitation = "";
  }
  pairingSession = null;
  pairingRelayAddress = null;
  render();
  try {
    snapshot = await cancelPairingSession();
    activeView = "devices";
    feedback = { tone: "success", message: "一次性邀请已清除，普通主机模式已恢复。" };
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
  const invitationText = currentPairingInvitation(snapshot, pairingSession);
  try {
    try {
      await navigator.clipboard.writeText(invitationText);
    } catch {
      const invitation = document.querySelector<HTMLTextAreaElement>("#pairing-invitation");
      if (!invitation) {
        throw new Error("配对邀请已不可用。");
      }
      invitation.select();
      if (!document.execCommand("copy")) {
        throw new Error("Windows 未允许 DeskLink 复制邀请。");
      }
    }
    feedback = { tone: "success", message: "配对连接码已复制，请在另一台 DeskLink 电脑的“控制电脑”页面完整粘贴。" };
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
      pairingSession.invitation = "";
      pairingSession = null;
      pairingRelayAddress = null;
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
window.setInterval(() => {
  if (!pairingSession || activeView !== "pairing") {
    return;
  }
  const countdown = document.querySelector<HTMLElement>("[data-pairing-countdown]");
  if (countdown) {
    countdown.textContent = formatPairingRemaining(pairingSession.expiresAtUnixS);
  }
  if (pairingSession.expiresAtUnixS <= Math.floor(Date.now() / 1000)) {
    pairingSession.invitation = "";
    pairingSession = null;
    pairingRelayAddress = null;
    feedback = { tone: "info", message: "一次性配对邀请已过期，并已从此窗口清除。" };
    render();
    void refreshSnapshot(false);
  }
}, 1000);
