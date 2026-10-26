import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { FileDropTargetGate, nativeFileDropHitsSurface } from "./file-drop-target";

const source = readFileSync(new URL("./controller.ts", import.meta.url), "utf8");
const initialize = source.slice(source.indexOf("async function initializeNativeFileDrop("), source.indexOf("\nfunction setFileDragActive("));
const enqueue = source.slice(source.indexOf("async function enqueueDroppedFiles("), source.indexOf("\nasync function removeQueuedFile("));

async function host() {
  const calls: unknown[][] = [];
  const content = { closest: () => null };
  const outside = { closest: () => null };
  let inSurface = true;
  const document = {
    hidden: false,
    elementFromPoint: () => inSurface ? content : outside,
    querySelectorAll: () => [{
      isConnected: true, getClientRects: () => [{}], contains: (element: unknown) => element === content,
      getBoundingClientRect: () => ({ left: 100, top: 50, right: 500, bottom: 350 }),
    }],
  };
  const create = new Function("FileDropTargetGate", "nativeFileDropHitsSurface", "document", "queueControllerFiles", `
    let listener, fileDropInitialized = false, dragActive = false;
    const fileDropTargetGate = new FileDropTargetGate();
    let snapshot = {runtime: {state: 'connected', streamId: 9}};
    let attemptTarget = '123 456 789', deviceIdDraft = '', channelGeneration = 3;
    const window = {devicePixelRatio: 1.5};
    const getCurrentWebview = () => ({onDragDropEvent: async callback => {listener = callback;}});
    const setFileDragActive = value => {dragActive = value;};
    const openTransferPanel = () => {}, updateTransferPanel = () => {}, showFileQueueError = () => {};
    let filePickerBusy = false, discardFileRecoveryBusy = false, discardFileQueueRecoveryBusy = false;
    const fileQueueActions = {busy:false};
    ${new Bun.Transpiler({ loader: "ts" }).transformSync(initialize + enqueue)}
    return {initialize: initializeNativeFileDrop, emit: payload => listener({payload}),
      active: () => dragActive, switchStream: () => {snapshot.runtime.streamId += 1; channelGeneration += 1;}};
  `);
  const flow = create(FileDropTargetGate, nativeFileDropHitsSurface, document, async (...args: unknown[]) => { calls.push(args); });
  await flow.initialize();
  return { flow, calls, setInSurface: (value: boolean) => { inSurface = value; } };
}

test("actual native drag callback shares hit testing for hint and drop and forwards captured ownership", async () => {
  const { flow, calls, setInSurface } = await host();
  const position = { x: 225, y: 150 };
  flow.emit({ type: "enter", position, paths: ["synthetic.txt"] });
  expect(flow.active()).toBe(true);
  setInSurface(false);
  flow.emit({ type: "over", position });
  expect(flow.active()).toBe(false);
  flow.emit({ type: "drop", position, paths: ["synthetic.txt"] });
  expect(calls).toEqual([]);
  setInSurface(true);
  flow.emit({ type: "enter", position, paths: ["synthetic.txt"] });
  flow.emit({ type: "drop", position, paths: ["synthetic.txt"] });
  expect(calls).toEqual([[["synthetic.txt"], 9, "123 456 789"]]);
  expect(flow.active()).toBe(false);
});

test("actual native callback rejects a drop after stream replacement and clears on leave", async () => {
  const { flow, calls } = await host();
  const position = { x: 225, y: 150 };
  flow.emit({ type: "enter", position, paths: ["synthetic.txt"] });
  flow.switchStream();
  flow.emit({ type: "drop", position, paths: ["synthetic.txt"] });
  expect(calls).toEqual([]);
  flow.emit({ type: "enter", position, paths: ["synthetic.txt"] });
  expect(flow.active()).toBe(true);
  flow.emit({ type: "leave" });
  expect(flow.active()).toBe(false);
  flow.emit({ type: "drop", position, paths: ["synthetic.txt"] });
  expect(calls).toEqual([]);
});
