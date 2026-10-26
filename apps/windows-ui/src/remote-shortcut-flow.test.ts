import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { remoteShortcutInput } from "./remote-shortcuts";

const source = readFileSync(new URL("./controller.ts", import.meta.url), "utf8");
const action = source.slice(source.indexOf("async function sendRemoteShortcut("), source.indexOf("\nasync function submitTextInput("));

function host(composing = false) {
  const sent: unknown[] = [];
  let resolve!: () => void;
  const wait = new Promise<void>((next) => { resolve = next; });
  const create = new Function("remoteShortcutInput", "send", "wait", "composing", `
    const snapshot = {runtime:{state:'connected',streamId:9}};
    let channelGeneration = 3, remoteShortcutBusy = false;
    const remoteTextInput = {state:{isComposing:composing}};
    let releaseCalls = 0, focusCalls = 0, errors = 0;
    const resetRemoteInputState = () => {releaseCalls += 1;};
    const focusRemoteKeyboard = () => {focusCalls += 1;};
    const showOperationError = () => {errors += 1;};
    const menu = {open:true}, buttons = [{disabled:false},{disabled:false}];
    const document = {querySelector:()=>menu, querySelectorAll:()=>buttons};
    const inputDispatcher = {enqueueShortcut:async (input, streamId) => {send(input,streamId); await wait;}};
    ${new Bun.Transpiler({loader:"ts"}).transformSync(action)}
    return {run:sendRemoteShortcut, switchStream:()=>{channelGeneration += 1; snapshot.runtime.streamId = 10;},
      state:()=>({releaseCalls,focusCalls,errors,busy:remoteShortcutBusy,menuOpen:menu.open,disabled:buttons.map(button=>button.disabled)})};
  `);
  const flow = create(remoteShortcutInput, (input: unknown, streamId: number) => { sent.push([input, streamId]); }, wait, composing);
  return { flow, sent, resolve };
}

test("actual shortcut action releases held keys, closes menu and serializes duplicate activation", async () => {
  const { flow, sent, resolve } = host();
  const action = flow.run("escape");
  await flow.run("alt-f4");
  expect(sent).toEqual([[remoteShortcutInput("escape"), 9]]);
  expect(flow.state()).toEqual({releaseCalls:1,focusCalls:0,errors:0,busy:true,menuOpen:false,disabled:[true,true]});
  resolve();
  await action;
  expect(flow.state().focusCalls).toBe(1);
  expect(flow.state().disabled).toEqual([false,false]);
});

test("actual shortcut action leaves active IME composition untouched", async () => {
  const { flow, sent } = host(true);
  await flow.run("escape");
  expect(sent).toEqual([]);
  expect(flow.state().releaseCalls).toBe(0);
});

test("completion from an old stream cannot take keyboard focus in the new session", async () => {
  const { flow, sent, resolve } = host();
  const action = flow.run("show-desktop");
  flow.switchStream();
  resolve();
  await action;
  expect(sent).toEqual([[remoteShortcutInput("show-desktop"), 9]]);
  expect(flow.state().focusCalls).toBe(0);
});
