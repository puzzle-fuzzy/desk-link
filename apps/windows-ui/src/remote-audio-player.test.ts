import { describe, expect, test } from "bun:test";
import { RemoteAudioPlayer } from "./remote-audio";

function payload(sequence: bigint, streamId = 1n, captureTimestampUs = sequence * 10_000n): Uint8Array {
  const bytes = new Uint8Array(988);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(0, streamId, true);
  view.setBigUint64(8, sequence, true);
  view.setBigUint64(16, captureTimestampUs, true);
  view.setUint32(24, 48_000, true);
  for (let offset = 28; offset < bytes.length; offset += 2) view.setInt16(offset, 16_384, true);
  return bytes;
}

function harness({ deferred = false, failed = false, suspended = false } = {}) {
  const messages: Record<string, unknown>[] = [];
  let resumeCount = 0;
  let nodeCount = 0;
  let closed = false;
  let disconnected = false;
  let resolveModule!: () => void;
  const moduleLoaded = deferred ? new Promise<void>((resolve) => { resolveModule = resolve; }) : Promise.resolve();
  const gain = { gain: { value: 1 }, connect() {}, disconnect() {} };
  const context = {
    state: suspended ? "suspended" : "running", currentTime: 1, destination: {}, onstatechange: null as (() => void) | null,
    createGain() { return gain; },
    async resume() { resumeCount += 1; if (suspended) throw new Error("activation required"); },
    async close() { closed = true; },
    createBuffer() { throw new Error("must not allocate AudioBuffer"); },
    createBufferSource() { throw new Error("must not allocate per-packet nodes"); },
  };
  const port = {
    onmessage: null as ((event: { data: unknown }) => void) | null,
    postMessage(message: Record<string, unknown>) { messages.push(message); }, close() {},
  };
  const node = { port, onprocessorerror: null as (() => void) | null, connect() {}, disconnect() { disconnected = true; } };
  const player = new RemoteAudioPlayer({
    createContext: () => context as unknown as AudioContext,
    createNode: () => { nodeCount += 1; return node as unknown as AudioWorkletNode; },
    loadModule: async () => { if (failed) throw new Error("unsupported"); await moduleLoaded; },
    now: () => context.currentTime * 1_000,
  });
  return { player, messages, node, context, gain, resolveModule, get nodeCount() { return nodeCount; }, get resumeCount() { return resumeCount; }, get closed() { return closed; }, get disconnected() { return disconnected; } };
}

async function settle(): Promise<void> { for (let index = 0; index < 6; index += 1) await Promise.resolve(); }

describe("远程声音 Worklet 播放生命周期", () => {
  test("单个播放节点接收循环 PCM 内存，不逐包创建 AudioBuffer 或 Source", async () => {
    const h = harness();
    h.player.prepare();
    await settle();
    expect(h.player.getSnapshot().state).toBe("running");
    for (let index = 1; index <= 100; index += 1) {
      h.context.currentTime += 0.01;
      h.player.push(payload(BigInt(index)));
      const sent = h.messages.at(-1)!;
      expect(sent.type).toBe("samples");
      expect((sent.samples as Float32Array)[0]).toBe(0.5);
      h.node.port.onmessage?.({ data: { type: "recycle", samples: sent.samples } });
    }
    expect(h.nodeCount).toBe(1);
    expect(h.player.getSnapshot().droppedPackets).toBe(0);
  });

  test("模块启动和 MessagePort 背压各自有固定 25 包上限", async () => {
    const h = harness({ deferred: true });
    h.player.prepare();
    for (let index = 1; index <= 100; index += 1) h.player.push(payload(BigInt(index)));
    expect(h.player.getSnapshot().droppedPackets).toBe(75);
    h.resolveModule();
    await settle();
    expect(h.messages.filter((message) => message.type === "samples").length).toBe(25);
    for (let index = 101; index <= 200; index += 1) h.player.push(payload(BigInt(index)));
    expect(h.messages.filter((message) => message.type === "samples").length).toBe(25);
    expect(h.player.getSnapshot().droppedPackets).toBe(175);
    // Dropped input does not generate an unbounded reset message backlog either.
    expect(h.messages.filter((message) => message.type === "reset").length).toBe(1);
  });

  test("stream、sequence、capture 断续均重置 Worklet 的代数", async () => {
    const h = harness(); h.player.prepare(); await settle();
    h.player.push(payload(1n));
    h.player.push(payload(3n));
    h.player.push(payload(4n, 2n));
    h.player.push(payload(5n, 2n, 1n));
    const resets = h.messages.filter((message) => message.type === "reset");
    expect(resets.map((message) => message.generation)).toEqual([0, 1, 2, 3]);
  });

  test("静音立即关增益并清缓冲；释放会话在模块迟到时不创建节点", async () => {
    const h = harness({ deferred: true }); h.player.prepare(); h.player.push(payload(1n));
    h.player.setEnabled(false);
    expect(h.gain.gain.value).toBe(0);
    expect(h.player.getSnapshot().state).toBe("muted");
    h.player.release(); h.resolveModule(); await settle();
    expect(h.nodeCount).toBe(0);
    expect(h.closed).toBe(true);
    expect(h.player.getSnapshot().state).toBe("closed");
  });

  test("真实暂停、启动失败与处理错误可见，数据包不反复请求激活", async () => {
    const paused = harness({ suspended: true }); paused.player.prepare(); await settle();
    expect(paused.player.getSnapshot().state).toBe("suspended");
    for (let index = 1; index <= 10; index += 1) paused.player.push(payload(BigInt(index)));
    expect(paused.resumeCount).toBe(1);
    const failed = harness({ failed: true }); failed.player.prepare(); await settle();
    expect(failed.player.getSnapshot().state).toBe("failed");
    const broken = harness(); broken.player.prepare(); await settle();
    broken.node.onprocessorerror?.();
    expect(broken.player.getSnapshot().state).toBe("failed");
    expect(broken.disconnected).toBe(true);
    broken.player.prepare(); await settle();
    expect(broken.nodeCount).toBe(2);
  });

  test("订阅真实播放与缓冲指标，旧代数指标不会覆盖 reset", async () => {
    const h = harness(); const states: string[] = [];
    const unsubscribe = h.player.subscribe((snapshot) => { states.push(snapshot.state); });
    h.player.prepare(); await settle();
    h.node.port.onmessage?.({ data: { type: "metrics", generation: 0, bufferedSamples: 480, underrunSamples: 128, overflowSamples: 10, staleSamples: 2, renderedSamples: 256 } });
    expect(h.player.getSnapshot().underrunSamples).toBe(128);
    h.player.resetConnection();
    h.node.port.onmessage?.({ data: { type: "metrics", generation: 0, bufferedSamples: 999 } });
    expect(h.player.getSnapshot().bufferedSamples).toBe(0);
    h.context.state = "suspended"; h.context.onstatechange?.();
    expect(states).toContain("suspended");
    unsubscribe();
    h.player.release();
    expect(states.at(-1)).toBe("suspended");
  });
});
