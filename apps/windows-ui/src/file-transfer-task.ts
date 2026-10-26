import { escapeHtml } from "./html";

export const MAX_FILE_TASK_VISIBLE_ITEMS = 64;
const U64_MAX = 18_446_744_073_709_551_615n;

export type FileTaskState = "scanning" | "waiting" | "sending" | "receiving" | "paused"
  | "verifying" | "completed" | "partial" | "failed" | "cancelled";
export type FileTaskItemState = Exclude<FileTaskState, "partial">;
export type FileTaskAction = "pause" | "resume" | "cancel" | "retry";

export interface FileTaskItem {
  id: string;
  path: string;
  kind?: "file" | "directory";
  state: FileTaskItemState;
  totalBytes: string;
  sentBytes: string;
  acknowledgedBytes: string;
  message?: string | null;
}

/** Bytes and revisions cross JSON as canonical decimal strings, including zero. */
export interface FileTaskSignal {
  kind: "fileTask";
  peerId?: string;
  taskId: string;
  revision: string;
  direction: "upload" | "download";
  name: string;
  state: FileTaskState;
  scanComplete: boolean;
  scannedItems: number;
  totalBytes: string;
  sentBytes: string;
  acknowledgedBytes: string;
  completedItems: number;
  failedItems: number;
  activeItem: FileTaskItem | null;
  items: FileTaskItem[];
  message?: string | null;
}

export interface FileCapabilitiesSignal {
  kind: "fileCapabilities";
  peerId?: string;
  state?: "pending" | "ready" | "unsupported";
  largeFiles: boolean;
  directories: boolean;
  blockResume: boolean;
}

export const NO_FILE_CAPABILITIES: FileCapabilitiesSignal = {
  kind: "fileCapabilities", largeFiles: false, directories: false, blockResume: false,
};

const states: ReadonlySet<string> = new Set<FileTaskState>([
  "scanning", "waiting", "sending", "receiving", "paused", "verifying",
  "completed", "partial", "failed", "cancelled",
]);

export function isTransferByteCount(value: unknown): value is string {
  return typeof value === "string" && /^(0|[1-9][0-9]{0,19})$/.test(value)
    && BigInt(value) <= U64_MAX;
}

export function isFileTaskId(value: unknown): value is string {
  return typeof value === "string" && /^[0-9a-f]{32}$/i.test(value) && value !== "0".repeat(32);
}

export function isFilePeerId(value: unknown): value is string {
  return typeof value === "string" && /^(?:[0-9a-f]{32}|[0-9a-f]{96})$/i.test(value) && !/^0+$/.test(value);
}

export function isFileTaskItemId(value: unknown): value is string {
  return isTransferByteCount(value) && value !== "0";
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function boundedText(value: unknown, limit: number): value is string {
  return typeof value === "string" && value.length <= limit && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f]/.test(value);
}

function validCounts(value: Record<string, unknown>): boolean {
  return isTransferByteCount(value.totalBytes) && isTransferByteCount(value.sentBytes)
    && isTransferByteCount(value.acknowledgedBytes)
    && BigInt(value.sentBytes) <= BigInt(value.totalBytes)
    && BigInt(value.acknowledgedBytes) <= BigInt(value.totalBytes);
}

function validItem(value: unknown): value is FileTaskItem {
  return isRecord(value) && isFileTaskItemId(value.id)
    && boundedText(value.path, 32_767) && value.path.length > 0
    && typeof value.state === "string" && states.has(value.state) && value.state !== "partial"
    && (value.kind === undefined || value.kind === "file" || value.kind === "directory")
    && (value.message === undefined || value.message === null || boundedText(value.message, 2_048)) && validCounts(value);
}

export function isFileTaskSignal(value: unknown): value is FileTaskSignal {
  if (!isRecord(value) || value.kind !== "fileTask"
    || (value.peerId !== undefined && !isFilePeerId(value.peerId))
    || !isFileTaskId(value.taskId)
    || !isTransferByteCount(value.revision)
    || (value.direction !== "upload" && value.direction !== "download")
    || !boundedText(value.name, 32_767) || typeof value.state !== "string" || !states.has(value.state)
    || typeof value.scanComplete !== "boolean"
    || ![value.scannedItems, value.completedItems, value.failedItems].every((count) =>
      typeof count === "number" && Number.isSafeInteger(count) && count >= 0 && count <= 0xffff_ffff)
    || !validCounts(value)
    || (value.message !== undefined && value.message !== null && !boundedText(value.message, 2_048))
    || !Array.isArray(value.items) || value.items.length > MAX_FILE_TASK_VISIBLE_ITEMS
    || !value.items.every(validItem) || (value.activeItem !== null && !validItem(value.activeItem))) {
    return false;
  }
  return new Set(value.items.map((item) => item.id)).size === value.items.length;
}

export class FileTaskStore {
  current: FileTaskSignal | null = null;
  #peerId: string | null = null;
  #newConnection = false;

  get peerId(): string | null { return this.#peerId; }

  beginPeer(peerId: unknown): boolean {
    if (!isFilePeerId(peerId)) return false;
    const canonical = peerId.toLowerCase();
    if (this.#peerId !== canonical) {
      this.#peerId = canonical;
      this.current = null;
      this.#newConnection = true;
    }
    return true;
  }

  beginConnection(): void { this.#newConnection = true; }

  accept(value: unknown): boolean {
    if (!isFileTaskSignal(value)) return false;
    if (this.#peerId !== null && value.peerId?.toLowerCase() !== this.#peerId) return false;
    if (!this.#newConnection && this.current?.taskId === value.taskId && BigInt(value.revision) <= BigInt(this.current.revision)) return false;
    // Only the bounded presentation window is retained; a directory manifest is
    // streamed and persisted by the native worker, never accumulated by WebView.
    this.current = { ...value, items: value.items.map((item) => ({ ...item })), activeItem: value.activeItem ? { ...value.activeItem } : null };
    this.#newConnection = false;
    return true;
  }
}

export function formatTaskBytes(value: string): string {
  if (!isTransferByteCount(value)) return "—";
  const bytes = BigInt(value);
  const units = ["B", "KB", "MB", "GB", "TB", "PB", "EB"];
  let unit = 0;
  let divisor = 1n;
  while (unit < units.length - 1 && bytes >= divisor * 1_024n) {
    divisor *= 1_024n;
    unit += 1;
  }
  if (unit === 0) return `${bytes} B`;
  const tenths = (bytes * 10n + divisor / 2n) / divisor;
  return `${tenths / 10n}.${tenths % 10n} ${units[unit]}`;
}

export function taskAcknowledgedPercent(task: Pick<FileTaskSignal, "totalBytes" | "acknowledgedBytes" | "state" | "scanComplete">): number | null {
  if (!task.scanComplete || !isTransferByteCount(task.totalBytes) || !isTransferByteCount(task.acknowledgedBytes)) return null;
  const total = BigInt(task.totalBytes);
  if (total === 0n) return task.state === "completed" ? 100 : 0;
  const percent = BigInt(task.acknowledgedBytes) * 100n / total;
  return Number(percent > 100n ? 100n : percent);
}

export function isFileTaskTerminal(state: FileTaskState): boolean {
  return state === "completed" || state === "partial" || state === "failed" || state === "cancelled";
}

export function fileTaskActions(state: FileTaskState): FileTaskAction[] {
  if (state === "paused") return ["resume", "cancel"];
  if (state === "partial" || state === "failed" || state === "cancelled") return ["retry"];
  if (state === "scanning" || state === "sending" || state === "receiving") return ["pause", "cancel"];
  if (state === "waiting" || state === "verifying") return ["cancel"];
  return [];
}

export function fileTaskStateLabel(state: FileTaskState): string {
  const labels: Record<FileTaskState, string> = {
    scanning: "正在扫描", waiting: "等待接收确认", sending: "正在发送", receiving: "正在接收",
    paused: "已暂停，可继续", verifying: "正在校验并保存", completed: "已校验并保存",
    partial: "部分完成", failed: "传输失败", cancelled: "已取消",
  };
  return labels[state];
}

export interface FileTaskViewOptions {
  busy: boolean;
  error: string;
  connected: boolean;
}

const actionLabels: Record<FileTaskAction, string> = { pause: "暂停", resume: "继续", cancel: "取消", retry: "重试失败项" };

export function renderFileTask(task: FileTaskSignal | null, options: FileTaskViewOptions): string {
  if (!task) return "";
  const percent = taskAcknowledgedPercent(task);
  const item = task.activeItem;
  const transferredLabel = task.direction === "upload" ? "已发送" : "已接收";
  const scannedLabel = task.direction === "upload" ? "已扫描" : "已发现";
  const controls = fileTaskActions(task.state).map((action) =>
    `<button type="button" data-controller-file-task-action="${action}" ${options.busy || !options.connected ? "disabled" : ""}>${actionLabels[action]}</button>`).join("");
  const itemViews = task.items.map((entry) => `<li data-file-task-item="${escapeHtml(entry.id)}" data-state="${entry.state}"><span><strong title="${escapeHtml(entry.path)}">${escapeHtml(entry.path)}</strong><small>${fileTaskStateLabel(entry.state)}${entry.kind === "directory" ? " · 文件夹" : ` · 已确认 ${formatTaskBytes(entry.acknowledgedBytes)} / ${formatTaskBytes(entry.totalBytes)}`}${entry.message ? ` · ${escapeHtml(entry.message)}` : ""}</small></span>${entry.state === "failed" ? `<button type="button" data-controller-file-task-retry-item="${escapeHtml(entry.id)}" ${options.busy || !options.connected ? "disabled" : ""}>重试此项</button>` : ""}</li>`).join("");
  return `<div class="remote-file-task-heading"><strong>${escapeHtml(task.name)}</strong><span>${fileTaskStateLabel(task.state)}</span></div>
    <p data-file-task-message>${escapeHtml(task.message ?? "")}</p>
    <p>${scannedLabel} ${task.scannedItems} 项${task.scanComplete ? "，清单完整" : "，总量仍在统计"} · 已完成 ${task.completedItems} 项${task.failedItems > 0 ? ` · 失败 ${task.failedItems} 项` : ""}</p>
    <progress max="100" ${percent === null ? "" : `value="${percent}"`} aria-label="接收端已确认的总进度"></progress>
    <p class="remote-file-task-bytes" title="累计传输包含已恢复的确认前缀；${transferredLabel} ${task.sentBytes} B；已确认 ${task.acknowledgedBytes} B；已发现总量 ${task.totalBytes} B">${transferredLabel} ${formatTaskBytes(task.sentBytes)} · 接收端已确认 ${formatTaskBytes(task.acknowledgedBytes)}${task.scanComplete ? ` / ${formatTaskBytes(task.totalBytes)}` : ""}</p>
    ${task.state === "verifying" ? "<p>正在核对完整内容并完成落盘，请等待最终结果。</p>" : ""}
    ${item ? `<p class="remote-file-task-current" title="${escapeHtml(item.path)}">当前：${escapeHtml(item.path)} · ${fileTaskStateLabel(item.state)} · ${transferredLabel} ${formatTaskBytes(item.sentBytes)} · 已确认 ${formatTaskBytes(item.acknowledgedBytes)} / ${formatTaskBytes(item.totalBytes)}</p>` : ""}
    <div class="remote-file-task-actions">${controls}${options.busy ? '<span role="status">正在处理…</span>' : ""}</div>
    <p class="remote-file-task-error" role="status">${escapeHtml(options.error)}</p>
    ${task.items.length > 0 ? `<details data-file-task-details><summary>查看${task.scannedItems > task.items.length ? "最近 " : ""}${task.items.length} 项${task.scannedItems > task.items.length ? `（共扫描 ${task.scannedItems} 项）` : ""}</summary><ul class="remote-file-task-items">${itemViews}</ul></details>` : ""}`;
}

type TaskFocus = { action?: string; retryItem?: string; summary?: boolean };
const retainedTaskFocus = new WeakMap<HTMLElement, TaskFocus>();

/** Repaints only this bounded region; the caller never replaces remote canvas. */
export function updateFileTaskRegion(root: HTMLElement, task: FileTaskSignal | null, options: FileTaskViewOptions): void {
  const previous = root.querySelector<HTMLDetailsElement>("[data-file-task-details]");
  const expanded = previous?.open ?? false;
  const scrollTop = previous?.querySelector<HTMLElement>("ul")?.scrollTop ?? 0;
  const focused = root.ownerDocument.activeElement as HTMLElement | null;
  const action = focused?.dataset?.controllerFileTaskAction;
  const retryItem = focused?.dataset?.controllerFileTaskRetryItem;
  const summary = focused !== null && focused === previous?.querySelector("summary");
  if (action || retryItem || summary) retainedTaskFocus.set(root, { action, retryItem, summary });
  else if (focused && focused !== root.ownerDocument.body) retainedTaskFocus.delete(root);
  const intendedFocus = retainedTaskFocus.get(root);
  root.hidden = task === null;
  root.innerHTML = renderFileTask(task, options);
  const details = root.querySelector<HTMLDetailsElement>("[data-file-task-details]");
  if (details) {
    details.open = expanded;
    const list = details.querySelector<HTMLElement>("ul");
    if (list) list.scrollTop = scrollTop;
  }
  if (intendedFocus?.summary) {
    details?.querySelector<HTMLElement>("summary")?.focus({ preventScroll: true });
  } else if (intendedFocus?.action || intendedFocus?.retryItem) {
    const controls = root.querySelectorAll<HTMLButtonElement>("button");
    const control = [...controls].find((button) => intendedFocus.action
      ? button.dataset.controllerFileTaskAction === intendedFocus.action
      : button.dataset.controllerFileTaskRetryItem === intendedFocus.retryItem);
    if (control && !control.disabled) control.focus({ preventScroll: true });
    else if (!control && !options.busy) {
      const next = [...controls].find((button) => !button.disabled);
      if (next) {
        next.focus({ preventScroll: true });
        retainedTaskFocus.set(root, { action: next.dataset.controllerFileTaskAction, retryItem: next.dataset.controllerFileTaskRetryItem });
      } else {
        details?.querySelector<HTMLElement>("summary")?.focus({ preventScroll: true });
        retainedTaskFocus.set(root, { summary: true });
      }
    }
  }
}
