import { expect, test } from "bun:test";
import { runInNewContext } from "node:vm";

test("实际 Worklet 在独立作用域消费 PCM，回收 transfer、丢弃旧代数及过期声音", async () => {
  const result = await Bun.build({ entrypoints: [import.meta.dir + "/remote-audio-worklet.ts"], target: "browser" });
  expect(result.success).toBe(true);
  const messages: Record<string, unknown>[] = [];
  type Processor = { port: { onmessage: (event: { data: unknown }) => void }; process(inputs: Float32Array[][], outputs: Float32Array[][]): boolean };
  let ProcessorConstructor: new (options: { processorOptions: { targetSamples: number } }) => Processor;
  const scope = {
    currentTime: 1, Float32Array,
    AudioWorkletProcessor: class {
      port = { onmessage: null, postMessage(message: Record<string, unknown>) { messages.push(message); } };
    },
    registerProcessor(name: string, constructor: typeof ProcessorConstructor) {
      expect(name).toBe("desklink-remote-audio"); ProcessorConstructor = constructor;
    },
  };
  runInNewContext(await result.outputs[0]!.text(), scope);
  const processor = new ProcessorConstructor!({ processorOptions: { targetSamples: 480 } });
  const pcm = new Float32Array(480).fill(0.25);
  processor.port.onmessage({ data: { type: "samples", generation: 0, samples: pcm, count: 480, queuedAt: 1 } });
  expect(messages[0]).toEqual({ type: "recycle", samples: pcm });
  const output = new Float32Array(128);
  // Audio callback proceeds without another main-thread / IPC event.
  for (let index = 0; index < 3; index += 1) {
    expect(processor.process([], [[output]])).toBe(true);
    expect(output.every((sample) => sample === 0.25)).toBe(true);
    scope.currentTime += 128 / 48_000;
  }
  processor.port.onmessage({ data: { type: "reset", generation: 1 } });
  processor.port.onmessage({ data: { type: "samples", generation: 0, samples: pcm, count: 480, queuedAt: scope.currentTime } });
  processor.port.onmessage({ data: { type: "samples", generation: 1, samples: pcm, count: 480, queuedAt: 0 } });
  for (let index = 0; index < 94; index += 1) {
    processor.process([], [[output]]);
    expect(output.every((sample) => sample === 0)).toBe(true);
  }
  const metrics = messages.find((message) => message.type === "metrics")!;
  expect(metrics.generation).toBe(1);
  expect(metrics.staleSamples).toBe(960);
  expect(metrics.bufferedSamples).toBe(0);
});
