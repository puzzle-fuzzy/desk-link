import { escapeHtml } from "./html";

export interface QueuedFileView {
  id: string;
  name: string;
  size: number;
}

interface FileQueueViewOptions {
  disabled: boolean;
  removingId: string | null;
  formatBytes: (size: number) => string;
  icon: (name: "file-up" | "loader-circle" | "x", className?: string) => string;
}

// Keep the native details control: Enter/Space and Tab work without a custom
// keyboard model, and every waiting file has its own stable removal identity.
export class FileQueueView {
  #expanded = false;
  #scrollTop = 0;
  #focused: { id: string; index: number } | "summary" | null = null;

  render(files: QueuedFileView[], options: FileQueueViewOptions): string {
    const list = `<ul class="remote-file-queue-items" data-controller-file-queue-items aria-label="全部等待发送的文件">${files.map((file, index) => {
      const removing = options.removingId === file.id;
      return `<li><span>${options.icon("file-up")}<span><strong title="${escapeHtml(file.name)}">${escapeHtml(file.name)}</strong><small>${options.formatBytes(file.size)}</small></span></span><button type="button" data-controller-file-queue-remove="${escapeHtml(file.id)}" aria-label="${removing ? "正在从队列移除" : "从队列移除"} 第 ${index + 1} 项 ${escapeHtml(file.name)}" title="${removing ? "正在移除" : "从队列移除"}" ${options.disabled ? "disabled" : ""}>${removing ? options.icon("loader-circle", "button-spinner") : options.icon("x")}</button></li>`;
    }).join("")}</ul>`;
    if (files.length <= 4) return list;
    return `<details class="remote-file-queue-details" data-controller-file-queue-details ${this.#expanded ? "open" : ""}><summary>查看全部 ${files.length} 个等待文件</summary>${list}</details>`;
  }

  capture(root: HTMLElement): void {
    const details = root.querySelector<HTMLDetailsElement>("[data-controller-file-queue-details]");
    if (details) this.#expanded = details.open;
    const list = root.querySelector<HTMLElement>("[data-controller-file-queue-items]");
    if (list) this.#scrollTop = list.scrollTop;
    const focused = root.ownerDocument.activeElement;
    const buttons = [...root.querySelectorAll<HTMLButtonElement>("[data-controller-file-queue-remove]")];
    const index = buttons.findIndex((button) => button === focused);
    const button = buttons[index];
    if (button) {
      this.#focused = { id: button.dataset.controllerFileQueueRemove!, index };
    } else if (focused === details?.querySelector("summary")) {
      this.#focused = "summary";
    } else if (focused && focused !== root.ownerDocument.body) {
      this.#focused = null;
    }
    // Replacing disabled controls temporarily moves focus to body. Retain the
    // intended row until the operation completes, unless the user focused elsewhere.
  }

  restore(root: HTMLElement): void {
    const details = root.querySelector<HTMLDetailsElement>("[data-controller-file-queue-details]");
    if (details) {
      details.open = this.#expanded;
      details.addEventListener("toggle", () => { this.#expanded = details.open; });
    }
    const list = root.querySelector<HTMLElement>("[data-controller-file-queue-items]");
    if (list) list.scrollTop = this.#scrollTop;
    if (!this.#focused) return;
    if (this.#focused === "summary") {
      details?.querySelector<HTMLElement>("summary")?.focus({ preventScroll: true });
      return;
    }
    const buttons = [...root.querySelectorAll<HTMLButtonElement>("[data-controller-file-queue-remove]")];
    const id = this.#focused.id;
    const button = buttons.find((item) => item.dataset.controllerFileQueueRemove === id)
      ?? buttons[Math.min(this.#focused.index, buttons.length - 1)];
    if (button && !button.disabled) {
      button.focus({ preventScroll: true });
    } else if (buttons.length === 0) {
      details?.querySelector<HTMLElement>("summary")?.focus({ preventScroll: true });
      this.#focused = null;
    }
  }
}
