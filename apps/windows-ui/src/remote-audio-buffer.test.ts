import { describe, expect, test } from "bun:test";
import { RemoteAudioBuffer } from "./remote-audio-buffer";

describe("远程声音有界缓冲", () => {
  test("达到目标提前量后连续跨环形边界播放，复用输出内存", () => {
    const buffer = new RemoteAudioBuffer(8, 4);
    const output = new Float32Array(3);
    buffer.push(new Float32Array([1, 2, 3]));
    buffer.render(output);
    expect([...output]).toEqual([0, 0, 0]);
    buffer.push(new Float32Array([4, 5, 6]));
    buffer.render(output);
    expect([...output]).toEqual([1, 2, 3]);
    buffer.push(new Float32Array([7, 8, 9]));
    buffer.render(output);
    expect([...output]).toEqual([4, 5, 6]);
    buffer.render(output);
    expect([...output]).toEqual([7, 8, 9]);
    expect(buffer.renderedSamples).toBe(9);
    expect(buffer.underrunSamples).toBe(0);
  });

  test("过载保留最新 PCM，容量及丢弃统计准确", () => {
    const buffer = new RemoteAudioBuffer(5, 1);
    buffer.push(new Float32Array([1, 2, 3]));
    buffer.push(new Float32Array([4, 5, 6, 7]));
    expect(buffer.bufferedSamples).toBe(5);
    expect(buffer.overflowSamples).toBe(2);
    const output = new Float32Array(5);
    buffer.render(output);
    expect([...output]).toEqual([3, 4, 5, 6, 7]);
    buffer.push(new Float32Array([1, 2, 3, 4, 5, 6, 7]));
    buffer.render(output);
    expect([...output]).toEqual([3, 4, 5, 6, 7]);
    expect(buffer.overflowSamples).toBe(4);
  });

  test("欠载填零并重新预缓冲，静音和重置不重放旧样本", () => {
    const buffer = new RemoteAudioBuffer(8, 3);
    const output = new Float32Array(4);
    buffer.push(new Float32Array([1, 2, 3]));
    buffer.render(output);
    expect([...output]).toEqual([1, 2, 3, 0]);
    expect(buffer.underrunSamples).toBe(1);
    buffer.push(new Float32Array([4, 5]));
    buffer.render(output);
    expect([...output]).toEqual([0, 0, 0, 0]);
    expect(buffer.underrunSamples).toBe(5);
    buffer.reset();
    buffer.push(new Float32Array([9, 8, 7]));
    buffer.render(output);
    expect([...output]).toEqual([9, 8, 7, 0]);
  });

  test("播放前过期检查禁止在 UI 或设备停顿后播放超过 250ms 的旧声音", () => {
    const buffer = new RemoteAudioBuffer(8, 2);
    buffer.push(new Float32Array([1, 2]), 2, 1);
    buffer.push(new Float32Array([3, 4]), 2, 1.2);
    const output = new Float32Array(2);
    buffer.render(output, 1.3);
    expect([...output]).toEqual([3, 4]);
    expect(buffer.staleSamples).toBe(2);
  });

  test("10 秒突发输入仍限定为 250ms，之后 128 样本回调连续输出最新内容", () => {
    const buffer = new RemoteAudioBuffer();
    const packet = new Float32Array(480).fill(0.5);
    for (let index = 0; index < 1_000; index += 1) buffer.push(packet);
    expect(buffer.bufferedSamples).toBe(12_000);
    expect(buffer.overflowSamples).toBe(468_000);
    const output = new Float32Array(128);
    for (let index = 0; index < 93; index += 1) {
      buffer.render(output);
      expect(output.every((sample) => sample === 0.5)).toBe(true);
    }
    expect(buffer.underrunSamples).toBe(0);
  });
});
