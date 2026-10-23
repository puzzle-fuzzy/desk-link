import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { escapeHtml } from "./html";
import { windowsUpdateProgressLabel, type WindowsUpdateState } from "./windows-update";

const source = readFileSync(resolve(import.meta.dir, "main.ts"), "utf8");
function functions(start: string, end: string): string {
  const offset = source.indexOf(start);
  return new Bun.Transpiler({ loader: "ts" }).transformSync(source.slice(offset, source.indexOf(end, offset)));
}
const presentation = functions("function windowsUpdateMarkup(", "\nfunction connectionToInput(");

function presentationHost() {
  let focused: ElementStub;
  class ElementStub {
    dataset: Record<string, string> = {};
    hidden = false;
    textContent = "";
    tabIndex = 0;
    focus() { focused = this; }
  }
  class ButtonStub extends ElementStub { disabled = false; }
  class ProgressStub extends ElementStub {
    max = 1;
    value: number | null = 0;
    removeAttribute(name: string) { if (name === "value") this.value = null; }
  }
  class Region extends ElementStub {
    writes = 0;
    children = new Map<string, ElementStub>();
    set innerHTML(markup: string) {
      if (this.contains(focused)) focused = body;
      this.writes += 1;
      this.children.clear();
      for (const match of markup.matchAll(/data-update-action="([^"]+)"/g)) {
        const button = new ButtonStub();
        button.dataset.updateAction = match[1]!;
        this.children.set(`[data-update-action="${match[1]}"]`, button);
      }
      for (const name of ["message", "cancel-error"]) {
        if (markup.includes(`data-update-${name}`)) this.children.set(`[data-update-${name}]`, new ElementStub());
      }
      if (markup.includes("data-update-progress")) this.children.set("[data-update-progress]", new ProgressStub());
    }
    contains(target: unknown) { return target === this || Array.from(this.children.values()).includes(target as ElementStub); }
    querySelector(selector: string) { return this.children.get(selector) ?? null; }
    querySelectorAll() { return Array.from(this.children.values()).filter((child) => child instanceof ButtonStub); }
    addEventListener() {}
  }
  const body = new ElementStub();
  const remoteInput = new ElementStub();
  focused = remoteInput;
  const banner = new Region();
  banner.dataset.windowsUpdateRegion = "banner";
  const settings = new Region();
  settings.dataset.windowsUpdateRegion = "settings";
  const updater = {
    state: { kind: "available", currentVersion: "0.1.92", latestVersion: "0.1.93", publishedAt: null } as WindowsUpdateState,
    bannerVisible: true,
  };
  const environment = {
    windowsUpdater: updater, windowsUpdateBanner: banner,
    windowsUpdateBoundRegions: new WeakSet(), applicationVersion: "0.1.92",
    escapeHtml, windowsUpdateProgressLabel,
    HTMLElement: ElementStub, HTMLButtonElement: ButtonStub,
    document: { get activeElement() { return focused; }, querySelectorAll: () => [banner, settings] },
  };
  const create = new Function(...Object.keys(environment), `${presentation}; return updateWindowsUpdateRegions;`);
  return { banner, settings, updater, remoteInput,
    update: create(...Object.values(environment)) as () => void,
    focus: () => focused,
  };
}

describe("isolated Windows update presentation", () => {
  test("progress patches retain update controls and leave the active connection/IME input untouched", () => {
    const host = presentationHost();
    host.update();
    host.updater.state = { kind: "installing", progress: { stage: "downloading", totalBytes: 100, downloadedBytes: 1 }, cancelling: false };
    host.update();
    const cancel = host.banner.querySelector('[data-update-action="cancel"]');
    const progress = host.banner.querySelector("[data-update-progress]");
    for (let value = 2; value <= 50; value += 1) {
      host.updater.state.progress.downloadedBytes = value;
      host.update();
    }
    expect(host.banner.writes).toBe(2);
    expect(host.settings.writes).toBe(2);
    expect(host.banner.querySelector('[data-update-action="cancel"]')).toBe(cancel);
    expect(host.banner.querySelector("[data-update-progress]")).toBe(progress);
    expect(host.banner.querySelector("[data-update-message]")?.textContent).toBe("正在下载 50%");
    expect(host.focus()).toBe(host.remoteInput);
    host.updater.state.progress = { stage: "verifying" };
    host.update();
    expect(cancel?.hidden).toBe(true);
    expect(progress?.hidden).toBe(true);
    expect(host.banner.writes).toBe(2);
  });

  test("keyboard focus stays in the update region through checking and returns to Retry on failure", () => {
    const host = presentationHost();
    host.update();
    host.banner.querySelector('[data-update-action="install"]')!.focus();
    host.updater.state = { kind: "installing", progress: { stage: "checking" }, cancelling: false };
    host.update();
    expect(host.focus()).toBe(host.banner);
    host.updater.state = { kind: "installError", latestVersion: "0.1.93", message: "下载失败，请重试。" };
    host.update();
    expect(host.focus()).toBe(host.banner.querySelector('[data-update-action="install"]')!);
  });

  test("startup checks begin before account restoration and online/focus recovery respects visibility", async () => {
    const operations = functions("async function bootstrap()", "\nfunction ensureWorkspaceHostListeners()");
    const actions: string[] = [];
    const events = new Map<string, () => void>();
    let restore!: (value: { signedIn: boolean }) => void;
    const document = { visibilityState: "visible", addEventListener: (name: string, callback: () => void) => events.set(name, callback) };
    const environment = {
      checkForWindowsUpdate: (force: boolean) => { expect(force).toBe(false); actions.push("check"); return Promise.resolve(); },
      window: { addEventListener: (name: string, callback: () => void) => events.set(name, callback) }, document,
      accountRestore: () => { actions.push("restore"); return new Promise((resolve) => { restore = resolve; }); },
      account: { signedIn: false }, accountLocalMode: false, accountLoading: true, accountError: null,
      shouldStartInLocalMode: () => false, render: () => {}, normalizeError: () => "",
      startWorkspace: () => { throw new Error("No workspace should be required for an update check"); },
    };
    const create = new Function(...Object.keys(environment), `${operations}; return bootstrap;`);
    const pending = create(...Object.values(environment))() as Promise<void>;
    expect(actions).toEqual(["check", "restore"]);
    events.get("online")!();
    expect(actions).toEqual(["check", "restore", "check"]);
    document.visibilityState = "hidden";
    events.get("focus")!();
    expect(actions).toEqual(["check", "restore", "check"]);
    restore({ signedIn: false });
    await pending;
  });

  test("native update installation accepts only a progress channel, and cancel preserves false", async () => {
    const api = readFileSync(resolve(import.meta.dir, "api.ts"), "utf8");
    const start = api.indexOf("export function installWindowsUpdate(");
    const end = api.indexOf("\nexport function quitDeskLink(", start);
    const bridge = new Bun.Transpiler({ loader: "ts" }).transformSync(api.slice(start, end).replaceAll("export function", "function"));
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    class Channel { onmessage?: (value: unknown) => void; }
    const invoke = async (command: string, args?: Record<string, unknown>) => {
      calls.push({ command, args });
      return command === "cancel_windows_update" ? false : undefined;
    };
    const methods = new Function("Channel", "invoke", `${bridge}; return { installWindowsUpdate, cancelWindowsUpdate };`)(Channel, invoke);
    const progress: unknown[] = [];
    await methods.installWindowsUpdate((value: unknown) => progress.push(value));
    expect(calls[0]!.command).toBe("install_windows_update");
    expect(Object.keys(calls[0]!.args!)).toEqual(["progress"]);
    (calls[0]!.args!.progress as Channel).onmessage!({ stage: "downloading", downloadedBytes: 20 });
    expect(progress).toEqual([{ stage: "downloading", downloadedBytes: 20 }]);
    expect(await methods.cancelWindowsUpdate()).toBe(false);
    expect(calls[1]).toEqual({ command: "cancel_windows_update", args: undefined });
  });
});
