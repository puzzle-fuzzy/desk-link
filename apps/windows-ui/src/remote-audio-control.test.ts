import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { remoteAudioControl } from "./remote-audio-control";

test("native enabled cannot conceal a suspended or failed local AudioContext", () => {
  for (const state of ["suspended", "failed", "closed"] as const) {
    expect(remoteAudioControl("enabled", true, "native playing", {state, message: "local playback needs attention"})).toMatchObject({retry: true, label: "重试声音", message: "local playback needs attention"});
  }
  expect(remoteAudioControl("unavailable", true, "unsupported", {state: "running", message: "running"})).toMatchObject({unavailable: true, retry: false});
});

test("actual retry action resumes local playback without muting the remote source", async () => {
  const source = readFileSync(new URL("./controller.ts", import.meta.url), "utf8");
  const action = source.slice(source.indexOf("async function toggleRemoteAudio("), source.indexOf("\nfunction updateAudioControl("));
  let prepared = 0, toggles = 0;
  const fn = new Function("remoteAudioControl", "remoteAudio", "setControllerAudioEnabled", `
    let audioToggleBusy = false, audioStatus = "enabled", audioEnabled = true, audioMessage = "native playing";
    const updateAudioControl = () => {};
    ${new Bun.Transpiler({loader: "ts"}).transformSync(action)}
    return toggleRemoteAudio;
  `)(remoteAudioControl, { getSnapshot: () => ({state: "suspended", message: "resume"}), prepare: () => {prepared++;} }, () => {toggles++;});
  await fn();
  expect(prepared).toBe(1);
  expect(toggles).toBe(0);
});
