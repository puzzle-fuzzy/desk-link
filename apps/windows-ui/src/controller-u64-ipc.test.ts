import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { validInputContext } from "./remote-input-context";
import { isU64Identity } from "./u64-identity";

const source = readFileSync(new URL("./api.ts", import.meta.url), "utf8");
const methods = ["sendControllerInput", "sendControllerText", "pasteControllerClipboardText", "nextControllerVideoFrame", "queueControllerFiles", "reportControllerRenderMetrics", "reportControllerPlaybackPressure"];
const operations = methods.map((name) => {
  const start = source.indexOf(`export function ${name}(`);
  const end = source.indexOf("\nexport function ", start + 1);
  return source.slice(start, end < 0 ? source.length : end).replace("export ", "");
}).join("\n");

function host() {
  const calls: Array<{ command: string; arguments: unknown }> = [];
  const create = new Function("invoke", "validInputContext", "isU64Identity", `${new Bun.Transpiler({loader:"ts"}).transformSync(operations)};return {${methods.join(",")}};`);
  const api = create(async (command: string, args: unknown) => { calls.push({command, arguments: args}); }, validInputContext, isU64Identity);
  return { api, calls };
}

test.each(["9007199254740991", "9007199254740992", "9007199254740993", "18446744073709551615"])("actual Windows IPC preserves full-u64 identity %s as decimal strings", async (streamId) => {
  const { api, calls } = host();
  const context = {streamId, inputEpoch:"18446744073709551615"};
  const input = {...context, kind:"key",key:"escape",pressed:true,modifiers:0};
  await api.sendControllerInput(input);
  await api.sendControllerText("中文",context);
  await api.pasteControllerClipboardText(context);
  await api.nextControllerVideoFrame({streamId,configVersion:7});
  await api.queueControllerFiles(["synthetic.txt"],streamId,"123 456 789");
  await api.reportControllerRenderMetrics({streamId});
  await api.reportControllerPlaybackPressure({streamId});
  expect(calls).toEqual([
    {command:"send_controller_input",arguments:{input}},
    {command:"send_controller_text",arguments:{text:"中文",...context}},
    {command:"paste_controller_clipboard_text",arguments:context},
    {command:"next_controller_video_frame",arguments:{input:{streamId,configVersion:7}}},
    {command:"queue_controller_files",arguments:{paths:["synthetic.txt"],streamId,deviceId:"123 456 789"}},
    {command:"report_controller_render_metrics",arguments:{metrics:{streamId}}},
    {command:"report_controller_playback_pressure",arguments:{pressure:{streamId}}},
  ]);
  expect(JSON.parse(JSON.stringify(calls))).toEqual(calls);
});

test.each([undefined, null, 0, 1, 9007199254740992, "0", "01", "+1", "-1", "1.5", " 1", "18446744073709551616"])("invalid identity %# never enters a Windows IPC request", async (invalid) => {
  const { api, calls } = host();
  await expect(api.sendControllerInput({kind:"key",key:"escape",pressed:true,streamId:invalid,inputEpoch:"1"})).rejects.toBeInstanceOf(Error);
  await expect(api.sendControllerText("text",{streamId:"1",inputEpoch:invalid})).rejects.toBeInstanceOf(Error);
  await expect(api.pasteControllerClipboardText({streamId:invalid,inputEpoch:"1"})).rejects.toBeInstanceOf(Error);
  await expect(api.nextControllerVideoFrame({streamId:invalid,configVersion:1})).rejects.toBeInstanceOf(Error);
  await expect(api.queueControllerFiles([],invalid,"123 456 789")).rejects.toBeInstanceOf(Error);
  await expect(api.reportControllerRenderMetrics({streamId:invalid})).rejects.toBeInstanceOf(Error);
  await expect(api.reportControllerPlaybackPressure({streamId:invalid})).rejects.toBeInstanceOf(Error);
  expect(calls).toEqual([]);
});
