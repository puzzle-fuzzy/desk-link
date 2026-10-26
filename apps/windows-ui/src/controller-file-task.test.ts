import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { escapeHtml } from "./html";
import { fileQueueProtectionPresentation } from "./file-queue-protection";
import {
  FileTaskStore, fileTaskActions, isFileTaskId, isFileTaskItemId,
  type FileTaskSignal,
} from "./file-transfer-task";

const taskId = "1234567890abcdef1234567890abcdef";
const anotherTaskId = "abcdef1234567890abcdef1234567890";
const controller = readFileSync(new URL("./controller.ts", import.meta.url), "utf8");
const apiSource = readFileSync(new URL("./api.ts", import.meta.url), "utf8");
const transpiler = new Bun.Transpiler({ loader: "ts" });
function operation(source: string, name: string): string {
  const start = source.search(new RegExp(`(?:export )?(?:async )?function ${name}\\(`));
  if (start < 0) throw new Error(`Missing actual operation ${name}`);
  const next = source.slice(start + 1).search(/\n(?:export )?(?:async )?function /);
  return source.slice(start, next < 0 ? source.length : start + next + 1).replace(/^export /, "");
}

function task(state: FileTaskSignal["state"] = "sending", id = taskId): FileTaskSignal {
  return {
    kind: "fileTask", taskId: id, revision: "1", direction: "upload", name: "测试目录", state,
    scanComplete: true, scannedItems: 2, completedItems: 0, failedItems: 1,
    totalBytes: "4294967297", sentBytes: "0", acknowledgedBytes: "0", activeItem: null,
    items: [{ id: "2", path: "中文/失败项.txt", state: "failed", totalBytes: "1", sentBytes: "0", acknowledgedBytes: "0" }],
  };
}

function actionHost(state: FileTaskSignal["state"] = "sending") {
  const store = new FileTaskStore();
  store.accept(task(state));
  const calls: Array<{ name: string; args: unknown[] }> = [];
  let resolve!: () => void;
  let reject!: (error: Error) => void;
  const pending = new Promise<void>((ok, error) => { resolve = ok; reject = error; });
  const environment = {
    fileTaskStore: store, fileTaskActions,
    snapshot: { runtime: { state: "connected" } },
    fileCapabilities: { largeFiles: true, blockResume: true },
    updateFileTaskPanel: () => {},
    normalizeError: (error: Error) => error.message,
    ...Object.fromEntries(["pause", "resume", "cancel", "retry"].map((name) => [
      `${name}ControllerFileTask`, (...args: unknown[]) => { calls.push({ name, args }); return pending; },
    ])),
  };
  const create = new Function(...Object.keys(environment), `let fileTaskOperation=null,fileTaskError="";
    ${transpiler.transformSync(operation(controller, "runFileTaskAction"))}
    return {runFileTaskAction,getError:()=>fileTaskError,isBusy:()=>fileTaskOperation!==null,
      disconnect:()=>{snapshot.runtime.state="stopped";}};`);
  return { ...create(...Object.values(environment)), store, calls, resolve, reject };
}

test("实际任务动作单飞，等待 native 接受后才解除忙碌状态", async () => {
  const host = actionHost();
  const first = host.runFileTaskAction("pause");
  expect(host.isBusy()).toBe(true);
  await host.runFileTaskAction("cancel");
  expect(host.calls).toEqual([{ name: "pause", args: [taskId] }]);
  host.resolve();
  await first;
  expect(host.isBusy()).toBe(false);
  // The UI does not invent a paused state before native progress confirms it.
  expect(host.store.current?.state).toBe("sending");
});

test("实际暂停恢复、取消和失败项重试保留任务身份", async () => {
  for (const [state, action, args] of [
    ["paused", "resume", [taskId]], ["verifying", "cancel", [taskId]],
    ["partial", "retry", [taskId, undefined]], ["sending", "retry", [taskId, "2"]],
  ] as const) {
    const host = actionHost(state);
    const pending = host.runFileTaskAction(action, args[1]);
    expect(host.calls).toEqual([{ name: action, args: [...args] }]);
    host.resolve();
    await pending;
  }
});

test("实际非法失败项、终态动作和断线动作都不进入 native", async () => {
  const failed = actionHost("partial");
  await failed.runFileTaskAction("retry", "not-a-failed-item");
  expect(failed.calls).toEqual([]);
  const complete = actionHost("completed");
  await complete.runFileTaskAction("pause");
  expect(complete.calls).toEqual([]);
  const disconnected = actionHost();
  disconnected.disconnect();
  await disconnected.runFileTaskAction("cancel");
  expect(disconnected.calls).toEqual([]);
});

test("实际动作错误只显示给所属任务，不能覆盖后续任务", async () => {
  const host = actionHost();
  const pending = host.runFileTaskAction("pause");
  host.store.accept(task("sending", anotherTaskId));
  host.reject(new Error("旧任务的迟到错误"));
  await pending;
  expect(host.getError()).toBe("");
  expect(host.isBusy()).toBe(false);
  const current = actionHost();
  const currentPending = current.runFileTaskAction("pause");
  current.reject(new Error("磁盘尚在处理，请重试"));
  await currentPending;
  expect(current.getError()).toBe("磁盘尚在处理，请重试");
});

test("实际目录选择在功能未协商时不打开选择器，忙碌时不重复打开", async () => {
  let selected = 0;
  let resolve!: () => void;
  const pending = new Promise<void>((ok) => { resolve = ok; });
  const environment = {
    fileCapabilities: { directories: false },
    fileTransferAvailable: () => true,
    fileTaskStore: new FileTaskStore(),
    fileQueueActions: { busy: false },
    openTransferPanel: () => {}, updateTransferPanel: () => {},
    showFileQueueError: () => {}, normalizeError: (error: Error) => error.message,
    chooseAndSendControllerDirectory: () => { selected += 1; return pending; },
  };
  const create = new Function(...Object.keys(environment), `let filePickerBusy=false,discardFileRecoveryBusy=false,discardFileQueueRecoveryBusy=false,fileTaskError="";
    ${transpiler.transformSync(operation(controller, "chooseDirectoryForTransfer"))}
    return {chooseDirectoryForTransfer,enable:()=>{fileCapabilities.directories=true;}};`);
  const host = create(...Object.values(environment));
  await host.chooseDirectoryForTransfer();
  expect(selected).toBe(0);
  host.enable();
  const first = host.chooseDirectoryForTransfer();
  await host.chooseDirectoryForTransfer();
  expect(selected).toBe(1);
  resolve();
  await first;
});

test("实际任务 IPC 保留逐项 identity 并在 native 前拒绝无效标识", async () => {
  const names = ["pauseControllerFileTask", "resumeControllerFileTask", "cancelControllerFileTask", "retryControllerFileTask"];
  const calls: Array<{ command: string; args: unknown }> = [];
  const create = new Function("invoke", "isFileTaskId", "isFileTaskItemId", `${transpiler.transformSync(names.map((name) => operation(apiSource, name)).join("\n"))};return {${names.join(",")}};`);
  const api = create(async (command: string, args: unknown) => { calls.push({ command, args }); }, isFileTaskId, isFileTaskItemId);
  await api.retryControllerFileTask(taskId, "18446744073709551615");
  expect(calls).toEqual([{ command: "retry_controller_file_task", args: { taskId, itemId: "18446744073709551615" } }]);
  for (const invalid of [null, 123, "", "../task", "a".repeat(33)]) {
    for (const name of names) await expect(api[name](invalid)).rejects.toBeInstanceOf(Error);
  }
  await expect(api.retryControllerFileTask(taskId, "2\u0000other")).rejects.toBeInstanceOf(Error);
  expect(calls).toHaveLength(1);
});

test("实际进度风暴只安排一次100ms任务区域更新，终态立即刷新", () => {
  let paints = 0;
  let scheduled = 0;
  let callback: (() => void) | null = null;
  const environment = {
    performance: { now: () => 20 },
    updateTransferToolbarActivity: () => {},
    updateFileTaskPanel: () => { paints += 1; },
    window: { setTimeout: (action: () => void, delay: number) => {
      scheduled += 1; callback = action; expect(delay).toBe(80); return 1;
    }, clearTimeout: () => { callback = null; } },
  };
  const create = new Function(...Object.keys(environment), `let transferPanelOpen=true,fileTaskPaintTimer=null,lastFileTaskPaintAtMs=0;
    ${transpiler.transformSync(operation(controller, "scheduleFileTaskPanelUpdate") + operation(controller, "cancelScheduledFileTaskUpdate"))}
    return {scheduleFileTaskPanelUpdate};`);
  const host = create(...Object.values(environment));
  for (let index = 0; index < 10_000; index += 1) host.scheduleFileTaskPanelUpdate(false);
  expect(scheduled).toBe(1);
  expect(paints).toBe(0);
  expect(callback).not.toBeNull();
  host.scheduleFileTaskPanelUpdate(true);
  expect(paints).toBe(1);
  expect(callback).toBeNull();
});

test("实际等待队列保存中和清空保存中都显示等待状态，不显示失败重试按钮", async () => {
  const environment = {
    fileQueue: { queued: [{ id: "1", name: "等待.txt", size: 1 }], recoveryState: "protecting", recoveryMessage: null },
    fileQueueActions: { busy: false, matches: () => false },
    fileQueueProtectionPresentation, escapeHtml,
    icon: (name: string) => `<i data-icon="${name}"></i>`,
  };
  const create = new Function(...Object.keys(environment), `${transpiler.transformSync(operation(controller, "renderFileQueueProtection") + operation(controller, "renderFileQueueRecovery"))};
    return {renderFileQueueProtection,clear:()=>{fileQueue.queued=[];return renderFileQueueRecovery();}};`);
  const host = create(...Object.values(environment));
  for (const html of [host.renderFileQueueProtection(), host.clear()]) {
    let retryButtons = 0;
    let statusRegions = 0;
    await new HTMLRewriter().on("button", { element() { retryButtons += 1; } })
      .on('[role="status"]', { element() { statusRegions += 1; } })
      .transform(new Response(html)).text();
    expect(retryButtons).toBe(0);
    expect(statusRegions).toBe(1);
  }
});

test("实际下载入口在目录完成或部分成功有落盘项时启用，失败零项不会启用", () => {
  const region = { hidden: true };
  const button = { disabled: false, innerHTML: "" };
  const message = { textContent: "" };
  const store = new FileTaskStore();
  const environment = {
    fileTransfer: null, transferHistory: [], fileTaskStore: store,
    document: { querySelector: (selector: string) => selector === ".remote-downloads-action" ? region
      : selector === "[data-controller-downloads-open]" ? button : message },
    downloadsFolderBusy: false, downloadsFolderMessage: "", icon: () => "", renderLucideIcons: () => {},
  };
  const create = new Function(...Object.keys(environment), `${transpiler.transformSync(operation(controller, "hasCompletedDownload") + operation(controller, "updateDownloadsFolderAction"))};return {updateDownloadsFolderAction};`);
  const host = create(...Object.values(environment));
  for (const [state, completed, visible] of [
    ["completed", 2, true], ["completed", 0, true], ["partial", 1, true], ["partial", 0, false], ["failed", 0, false], ["receiving", 1, false],
  ] as const) {
    store.current = { ...task(state), direction: "download", completedItems: completed };
    host.updateDownloadsFolderAction();
    expect(region.hidden).toBe(!visible);
    expect(button.disabled).toBe(false);
  }
});

test("实际选文件和双向选择请求在准备中或不支持时都不进入 native", async () => {
  const calls: string[] = [];
  const capabilities = { state: "pending", largeFiles: false, directories: false, blockResume: false };
  const environment = {
    fileCapabilities: capabilities,
    chooseAndSendControllerFile: () => { calls.push("file"); },
    chooseAndSendControllerDirectory: () => { calls.push("directory"); },
    requestControllerRemoteFile: () => { calls.push("remote-file"); },
    requestControllerRemoteDirectory: () => { calls.push("remote-directory"); },
  };
  const methods = ["fileTransferAvailable", "fileCapabilityMessage", "chooseFileForTransfer", "chooseDirectoryForTransfer", "requestRemoteFileTransfer", "requestRemoteDirectoryTransfer"];
  const create = new Function(...Object.keys(environment), `${transpiler.transformSync(methods.map((name) => operation(controller, name)).join("\n"))};return {${methods.join(",")}};`);
  const host = create(...Object.values(environment));
  for (const state of ["pending", "unsupported"]) {
    capabilities.state = state;
    expect(host.fileTransferAvailable()).toBe(false);
    expect(host.fileCapabilityMessage()).toBe(state === "pending" ? "正在准备文件传输，请稍候。" : "此连接暂不支持文件传输。");
    await host.chooseFileForTransfer();
    await host.chooseDirectoryForTransfer();
    await host.requestRemoteFileTransfer();
    await host.requestRemoteDirectoryTransfer();
  }
  expect(calls).toEqual([]);
});
