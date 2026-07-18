import { describe, expect, test } from "bun:test";

import { decodeRemoteAudioPayload } from "./remote-audio";

function packet(payload: number[], sampleRate = 48_000): Uint8Array {
  const bytes = new Uint8Array(28 + payload.length);
  const view = new DataView(bytes.buffer);
  view.setBigUint64(0, 7n, true);
  view.setBigUint64(8, 11n, true);
  view.setBigUint64(16, 1_000_000n, true);
  view.setUint32(24, sampleRate, true);
  bytes.set(payload, 28);
  return bytes;
}

describe("远程系统声音数据", () => {
  test("解析固定头与有符号 16 位 PCM", () => {
    const decoded = decodeRemoteAudioPayload(packet([0, 128, 255, 127]));
    expect(decoded.streamId).toBe(7n);
    expect(decoded.sequence).toBe(11n);
    expect(decoded.samples.length).toBe(2);
    expect(decoded.samples[0]).toBe(-1);
    expect(decoded.samples[1]).toBeCloseTo(32_767 / 32_768, 6);
  });

  test("拒绝奇数字节和非 48 kHz 数据", () => {
    expect(() => decodeRemoteAudioPayload(packet([0]))).toThrow();
    expect(() => decodeRemoteAudioPayload(packet([0, 0], 44_100))).toThrow();
  });

  test("拒绝超过单个音频包上限的数据", () => {
    expect(() => decodeRemoteAudioPayload(packet(new Array(962).fill(0)))).toThrow();
  });
});
