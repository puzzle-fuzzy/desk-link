import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { sameInputContext } from "./remote-input-context";
import type { RemoteInputContext } from "./types";

const source = readFileSync(new URL("./controller.ts", import.meta.url), "utf8");
const operation = source.slice(source.indexOf("async function pasteLocalClipboardText("), source.indexOf("\nfunction fireInput("));
const clipboardSignal = source.slice(source.indexOf('    case "clipboard":'), source.indexOf('    case "fileTransfer":', source.indexOf('    case "clipboard":')));

function host() {
  const calls: RemoteInputContext[] = [];
  let resolve!: () => void;
  const drained = new Promise<void>((next) => { resolve = next; });
  const create = new Function("sameInputContext", "drained", "paste", `
    let context = Object.freeze({streamId: "9",inputEpoch: "2"}), channelGeneration = 3;
    let smartPasteBusy = false, smartPastePending = false, clipboardTransfer = null, focusCalls = 0;
    const currentInputContext = () => context;
    const inputDispatcher = {drain:()=>drained,discardPendingMoves:()=>{}};
    const resetRemoteInputState = () => {}, updateTransferPanel = () => {}, openTransferPanel = () => {};
    const normalizeError = error => error.message, focusRemoteKeyboard = () => {focusCalls += 1;};
    const pasteControllerClipboardText = paste;
    ${new Bun.Transpiler({loader:"ts"}).transformSync(operation)}
    ${new Bun.Transpiler({loader:"ts"}).transformSync(`function acceptSignal(signal:any) {switch(signal.kind) {${clipboardSignal}}}`)}
    return {paste:pasteLocalClipboardText, signal:acceptSignal, pause:()=>{context=null;},
      changeEpoch:()=>{context=Object.freeze({streamId: "9",inputEpoch: "3"});},
      changeSession:()=>{channelGeneration += 1; context=Object.freeze({streamId: "10",inputEpoch: "1"}); smartPasteBusy=false;},
      state:()=>({smartPasteBusy,smartPastePending,clipboardTransfer,focusCalls})};
  `);
  return { ...create(sameInputContext, drained, async (context: RemoteInputContext) => { calls.push(context); }), calls, resolve };
}

test("actual Ctrl+V captures displayed context before waiting for old input to drain", async () => {
  const flow = host();
  const paste = flow.paste();
  expect(flow.calls).toEqual([]);
  flow.resolve();
  await paste;
  expect(flow.calls).toEqual([{streamId: "9",inputEpoch: "2"}]);
});

test("actual Ctrl+V cannot retag a pending clipboard read to a new epoch", async () => {
  const flow = host();
  const paste = flow.paste();
  flow.changeEpoch();
  flow.resolve();
  await paste;
  expect(flow.calls).toEqual([]);
  expect(flow.state().clipboardTransfer.state).toBe("failed");
  expect(flow.state().focusCalls).toBe(0);
});

test("new config before first painted frame and a replaced session both cancel deferred paste", async () => {
  for (const change of ["pause", "changeSession"] as const) {
    const flow = host();
    const paste = flow.paste();
    flow[change]();
    flow.resolve();
    await paste;
    expect(flow.calls).toEqual([]);
    expect(flow.state().focusCalls).toBe(0);
  }
});

test("DL2-047 actual paste completion clears busy state so a second paste can proceed", async () => {
  const flow = host();
  flow.resolve();
  await flow.paste();
  expect(flow.state().smartPasteBusy).toBe(true);
  expect(flow.calls).toHaveLength(1);
  flow.signal({kind:"clipboard",operation:"send",state:"completed",message:"unrelated clipboard action"});
  expect(flow.state().smartPasteBusy).toBe(true);
  flow.signal({kind:"clipboard",operation:"paste",state:"completed",message:"paste completed"});
  expect(flow.state().smartPasteBusy).toBe(false);
  expect(flow.state().smartPastePending).toBe(false);
  await flow.paste();
  expect(flow.calls).toHaveLength(2);
  flow.signal({kind:"clipboard",operation:"paste",state:"failed",message:"paste failed"});
  expect(flow.state().smartPasteBusy).toBe(false);
  await flow.paste();
  expect(flow.calls).toHaveLength(3);
});
