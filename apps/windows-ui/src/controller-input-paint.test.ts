import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { RemoteInputPresentation } from "./remote-input-context";
import { videoConfigKey } from "./video-config";
import type { ControllerVideoConfigSignal } from "./types";

const source = readFileSync(new URL("./controller.ts", import.meta.url), "utf8");
const start = source.indexOf("function inputSurfaceVisible(");
const end = source.indexOf("\nasync function startVideoDecoder(", start);
const operations = new Bun.Transpiler({ loader: "ts" }).transformSync(source.slice(start, end));
const initial: ControllerVideoConfigSignal = {
  kind: "videoConfig", streamId: "41", inputEpoch: "7", configVersion: 3,
  width: 1280, height: 720, sequenceHeader: [],
};

function paintHost() {
  let sequence = 0;
  const pending = new Map<number, () => void>();
  const callbacks = new Map<number, () => void>();
  const cancelled: number[] = [];
  const drawCalls: unknown[][] = [];
  const failures: unknown[][] = [];
  const gate = new RemoteInputPresentation();
  const hint = { textContent: "" };
  const waiting = { hidden: false };
  let visibleRects = true;
  let rectWidth = 1280;
  let rectHeight = 720;
  let layoutReads = 0;
  let throwLayout = false;
  let throwDraw = false;
  const canvas = {
    width: 1280, height: 720, isConnected: true,
    getClientRects: () => {
      layoutReads += 1;
      if (throwLayout) throw new Error("unexpected hot-path layout read");
      return visibleRects ? [{ width: rectWidth, height: rectHeight }] : [];
    },
  };
  const document = { hidden: false, querySelector: () => waiting };
  const viewport = { dataset: {} as Record<string, string>, querySelector: () => hint };
  const window = {
    requestAnimationFrame(callback: () => void) {
      sequence += 1;
      pending.set(sequence, callback);
      callbacks.set(sequence, callback);
      return sequence;
    },
    cancelAnimationFrame(id: number) { cancelled.push(id); pending.delete(id); },
  };
  const context = {
    drawImage(...args: unknown[]) {
      if (throwDraw) throw new Error("synthetic draw failure");
      drawCalls.push(args);
    },
  };
  const create = new Function("inputPresentation", "videoConfigKey", "document", "window", "canvas", "context", "viewport", "initial", "failed", `
    let remoteCanvasElement = canvas, remoteViewportElement = viewport, remotePanMode = false;
    let snapshot = {runtime: {state:'connected',streamId:"41"}};
    let inputActivationFrame = null, channelGeneration = 2, decoderGeneration = 5;
    let inputSurfaceConfirmedVisible = false;
    let videoConfig = {...initial}, remoteCanvasBounds = null, remoteViewportBounds = null;
    let decodedFrames = 0, videoConfigReceivedAtMs = Date.now(), firstFrameMs = null, consecutiveDecoderStalls = 1;
    const syncRemoteCanvasPixelSize = () => {}, scheduleRemoteScaleLayout = () => {};
    const videoRenderTiming = {observe: () => {}}, reportRenderMetrics = () => {};
    const fallbackOrFailVideo = (...args) => failed(...args);
    ${operations}
    inputPresentation.announce(videoConfig,channelGeneration);
    return {
      paint(frame, configuration = initial, connection = 2, decoder = 5) {
        paintRemoteVideoFrame(frame,canvas,context,configuration,connection,decoder,videoConfigKey(configuration),'hardware');
      },
      announce(configuration) {videoConfig = {...configuration}; return inputPresentation.announce(videoConfig,channelGeneration);},
      replaceChannel() {channelGeneration += 1;},
      stop() {snapshot.runtime.state = 'stopped'; snapshot.runtime.streamId = null;},
      replaceRuntimeStream(streamId) {snapshot.runtime.streamId = streamId;},
      restartDecoder() {decoderGeneration += 1;},
      clear() {inputPresentation.clear();},
      context: currentInputContext,
      present: schedulePresentedInputActivation,
      state: () => ({decodedFrames,firstFrameMs,inputActivationFrame})
    };
  `);
  const flow = create(gate, videoConfigKey, document, window, canvas, context, viewport, initial,
    (...args: unknown[]) => failures.push(args));
  function runFrame() {
    const batch = [...pending];
    for (const [id] of batch) pending.delete(id);
    for (const [, callback] of batch) callback();
  }
  function frame(width = 1280, height = 720) {
    return { displayWidth: width, displayHeight: height, closed: 0, close() { this.closed += 1; } };
  }
  return {
    flow, gate, canvas, document, hint, viewport, pending, callbacks, cancelled, drawCalls, failures, runFrame, frame,
    schedulePaint(value: ReturnType<typeof frame>, configuration = initial) {
      return window.requestAnimationFrame(() => flow.paint(value, configuration));
    },
    setRects: (visible: boolean) => { visibleRects = visible; },
    setRectSize: (width: number, height: number) => { rectWidth = width; rectHeight = height; },
    layoutReads: () => layoutReads,
    setThrowLayout: (throwing: boolean) => { throwLayout = throwing; },
    setThrow: (throwing: boolean) => { throwDraw = throwing; },
  };
}

describe("真实远程画面绘制的输入门禁", () => {
  test("真实drawImage成功后的同一帧仍暂停，下一个rAF屏障才启用", () => {
    const host = paintHost();
    const frame = host.frame();
    host.schedulePaint(frame);
    expect(host.flow.context()).toBeNull();
    host.runFrame();
    expect(host.drawCalls).toEqual([[frame, 0, 0]]);
    expect(host.gate.painted?.frameToken).toBe(frame);
    expect(frame.closed).toBe(1);
    expect(host.flow.context()).toBeNull();
    expect(host.pending.size).toBe(1);
    host.runFrame();
    expect(host.flow.context()).toEqual({ streamId: "41", inputEpoch: "7" });
    expect(host.viewport.dataset.inputReady).toBe("true");
    expect(host.flow.state().decodedFrames).toBe(1);
  });

  test("drawImage失败不产生可激活帧并走真实解码回退", () => {
    const host = paintHost();
    host.setThrow(true);
    const frame = host.frame();
    host.schedulePaint(frame);
    host.runFrame();
    host.runFrame();
    expect(host.gate.painted).toBeNull();
    expect(host.flow.context()).toBeNull();
    expect(host.failures).toEqual([["41:3", "hardware", "decoderError"]]);
    expect(frame.closed).toBe(1);
    expect(host.flow.state().decodedFrames).toBe(0);
  });

  test.each(["hidden", "rectless", "zero-width", "zero-height", "zero-area", "disconnected", "stopped", "runtime-stream", "channel"])(
    "绘制后屏障前发生%s时拒绝启用", (change) => {
      const host = paintHost();
      const frame = host.frame();
      host.flow.paint(frame);
      if (change === "hidden") host.document.hidden = true;
      else if (change === "rectless") host.setRects(false);
      else if (change === "zero-width") host.setRectSize(0, 720);
      else if (change === "zero-height") host.setRectSize(1280, 0);
      else if (change === "zero-area") host.setRectSize(0, 0);
      else if (change === "disconnected") host.canvas.isConnected = false;
      else if (change === "stopped") host.flow.stop();
      else if (change === "runtime-stream") host.flow.replaceRuntimeStream("42");
      else host.flow.replaceChannel();
      host.runFrame();
      expect(host.flow.context()).toBeNull();
      expect(host.viewport.dataset.inputReady).toBe("false");
    },
  );

  test.each(["channel", "decoder", "disconnected", "configuration", "epoch"])(
    "旧%s绘制回调只释放VideoFrame，不绘制或开放输入", (change) => {
      const host = paintHost();
      const frame = host.frame();
      if (change === "channel") host.flow.replaceChannel();
      else if (change === "decoder") host.flow.restartDecoder();
      else if (change === "disconnected") host.canvas.isConnected = false;
      else if (change === "configuration") host.flow.announce({ ...initial, configVersion: 4 });
      else host.flow.announce({ ...initial, inputEpoch: "8" });
      host.flow.paint(frame);
      host.runFrame();
      expect(host.drawCalls).toEqual([]);
      expect(host.gate.painted).toBeNull();
      expect(host.flow.context()).toBeNull();
      expect(frame.closed).toBe(1);
    },
  );

  test("新配置公告使已经排队的旧绘制屏障失效", () => {
    const host = paintHost();
    host.flow.paint(host.frame());
    host.flow.announce({ ...initial, inputEpoch: "8", configVersion: 4 });
    host.runFrame();
    expect(host.flow.context()).toBeNull();
    host.flow.paint(host.frame(), { ...initial, inputEpoch: "8", configVersion: 4 });
    expect(host.flow.context()).toBeNull();
    host.runFrame();
    expect(host.flow.context()).toEqual({ streamId: "41", inputEpoch: "8" });
  });

  test("新VideoFrame替代token后，即便被取消的旧屏障仍到达也不能启用", () => {
    const host = paintHost();
    const first = host.frame();
    const second = host.frame();
    host.flow.paint(first);
    const firstBarrier = host.flow.state().inputActivationFrame as number;
    host.flow.paint(second);
    expect(host.cancelled).toContain(firstBarrier);
    expect(host.gate.painted?.frameToken).toBe(second);
    host.callbacks.get(firstBarrier)!();
    expect(host.flow.context()).toBeNull();
    host.runFrame();
    expect(host.flow.context()).toEqual({ streamId: "41", inputEpoch: "7" });
    expect(first.closed).toBe(1);
    expect(second.closed).toBe(1);
  });

  test("取消clear后迟到的真实rAF回调不会恢复输入", () => {
    const host = paintHost();
    host.flow.paint(host.frame());
    host.flow.clear();
    host.runFrame();
    expect(host.gate.painted).toBeNull();
    expect(host.flow.context()).toBeNull();
    expect(host.viewport.dataset.inputReady).toBe("false");
  });

  test("已激活的画面隐藏后立即暂停，恢复可见仍需再过真实rAF屏障", () => {
    const host = paintHost();
    host.flow.paint(host.frame());
    host.runFrame();
    expect(host.flow.context()).toEqual({ streamId: "41", inputEpoch: "7" });
    host.document.hidden = true;
    expect(host.flow.context()).toBeNull();
    host.document.hidden = false;
    expect(host.flow.context()).toBeNull();
    host.flow.present();
    expect(host.flow.context()).toBeNull();
    host.runFrame();
    expect(host.flow.context()).toEqual({ streamId: "41", inputEpoch: "7" });
  });

  test("稳定的第二次真实绘制保持context且不额外排输入激活rAF", () => {
    const host = paintHost();
    const first = host.frame();
    host.flow.paint(first);
    host.runFrame();
    const context = host.flow.context();
    const candidate = host.gate.painted;
    const second = host.frame();
    host.flow.paint(second);
    expect(host.drawCalls).toEqual([[first, 0, 0], [second, 0, 0]]);
    expect(host.pending.size).toBe(0);
    expect(host.flow.context()).toBe(context);
    expect(host.gate.painted).toBe(candidate);
    expect(second.closed).toBe(1);
  });

  test("隐藏暂停后恢复使用稳定绘制的最新真实frameToken，旧屏障不能复活", () => {
    const host = paintHost();
    const first = host.frame();
    host.flow.paint(first);
    const firstBarrier = host.flow.state().inputActivationFrame as number;
    host.runFrame();
    const original = host.gate.painted;
    const latest = host.frame();
    host.flow.paint(latest);
    expect(host.pending.size).toBe(0);
    host.document.hidden = true;
    expect(host.flow.context()).toBeNull();
    expect(host.gate.painted).not.toBe(original);
    expect(host.gate.painted?.frameToken).toBe(latest);
    expect(original?.frameToken).toBe(first);
    host.document.hidden = false;
    host.flow.present();
    host.callbacks.get(firstBarrier)!();
    expect(host.flow.context()).toBeNull();
    host.runFrame();
    expect(host.flow.context()).toEqual({ streamId: "41", inputEpoch: "7" });
  });

  test("currentInputContext热点只读可见性缓存，不触发布局查询", () => {
    const host = paintHost();
    host.flow.paint(host.frame());
    expect(host.layoutReads()).toBe(0);
    host.runFrame();
    expect(host.layoutReads()).toBe(1);
    const context = host.flow.context();
    host.setThrowLayout(true);
    for (let index = 0; index < 100; index += 1) expect(host.flow.context()).toBe(context);
    host.flow.paint(host.frame());
    expect(host.flow.context()).toBe(context);
    expect(host.layoutReads()).toBe(1);
    expect(host.pending.size).toBe(0);
  });
});
