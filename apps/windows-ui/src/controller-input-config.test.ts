import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { RemoteInputPresentation } from "./remote-input-context";
import { RemoteKeyboardState } from "./remote-keyboard-state";
import { videoConfigKey } from "./video-config";
import type { ControllerVideoConfigSignal } from "./types";

const source = readFileSync(new URL("./controller.ts", import.meta.url), "utf8");
const handler = source.slice(source.indexOf('    case "videoConfig": {'), source.indexOf('    case "cursor":', source.indexOf('    case "videoConfig": {')));
const identity = source.slice(source.indexOf("export function controllerSurfaceIdentity("), source.indexOf("\nexport function updatePersistentControllerRegions("));
const initial: ControllerVideoConfigSignal = { kind: "videoConfig", streamId: "9", inputEpoch: "2", configVersion: 1, width: 100, height: 100, sequenceHeader: [1] };

function host() {
  const inputPresentation = new RemoteInputPresentation();
  inputPresentation.announce(initial, 3);
  const frame = inputPresentation.drew(initial, 3, {})!;
  inputPresentation.activate(frame, 3, "9", true);
  const held = new RemoteKeyboardState();
  held.press("ControlLeft", { kind: "key", key: "control", pressed: true, modifiers: 0 });
  const events: unknown[] = [];
  const create = new Function("inputPresentation", "videoConfigKey", "initial", "held", "events", `
    "use strict";
    let videoConfig = initial, channelGeneration = 3, pointerFrame = null, inputActivationFrame = null;
    let inputSurfaceConfirmedVisible = true;
    let loading = false, snapshot = {runtime:{state:'connected',streamId: "9"}}, remoteSurfaceState = 'live', failedVideoConfig = null;
    let videoSequenceHeader, videoConfigReceivedAtMs, highProfileFallbackRequested, h264ProfileProbe, h264ProfileProbeMs, h264ProfileFallbackReason;
    const inputDispatcher = {discardPendingMoves:()=>events.push('moves'),discardAll:()=>events.push('discardAll')};
    const resetRemoteInputState = () => {events.push('releaseHeld'); held.releaseAll();};
    const resetVideoTelemetry = () => {}, updateInputContextPresentation = () => {};
    const setupRemoteDesktop = flag => events.push(['configureInPlace',flag]);
    const requestRender = () => events.push('render');
    const document = {querySelector:()=>({})}, window = {cancelAnimationFrame:()=>{}};
    ${new Bun.Transpiler({loader:"ts"}).transformSync(identity.replace("export ", "") + `\nfunction deliver(signal:any) {switch(signal.kind) {${handler}}}`)}
    return {signal:deliver, identity:controllerSurfaceIdentity};
  `);
  return { ...create(inputPresentation, videoConfigKey, initial, held, events), inputPresentation, held, events };
}

test("actual config handler preserves held keys and DOM for same-epoch quality changes", () => {
  const flow = host();
  const identity = flow.identity();
  flow.signal({ ...initial, configVersion: 2, width: 200 });
  expect(flow.identity()).toBe(identity);
  expect(flow.inputPresentation.context).toBeNull();
  expect(flow.held.modifierMask()).toBe(2);
  expect(flow.events).toEqual(["moves", ["configureInPlace", false]]);
});

test("actual epoch change releases old held state and discards old pending work before in-place configuration", () => {
  const flow = host();
  flow.signal({ ...initial, inputEpoch: "3", configVersion: 2 });
  expect(flow.inputPresentation.context).toBeNull();
  expect(flow.held.modifierMask()).toBe(0);
  expect(flow.events).toEqual(["moves", "discardAll", "releaseHeld", ["configureInPlace", false]]);
});
