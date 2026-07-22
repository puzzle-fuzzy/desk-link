import { describe, expect, test } from "bun:test";
import { readLittleEndianTimestamp } from "./video-timestamp";

describe("video timestamp decoding", () => {
  test("reads the little-endian u64 used by the frame envelope", () => {
    const bytes = Uint8Array.from([
      0xaa,
      0x78, 0x56, 0x34, 0x12,
      0xef, 0xcd, 0xab, 0x90,
    ]);
    expect(readLittleEndianTimestamp(bytes, 1)).toBe(0x90abcdef12345678);
  });

  test("supports an offset inside a larger frame envelope", () => {
    const bytes = Uint8Array.from([0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
    expect(readLittleEndianTimestamp(bytes, 2)).toBe(1);
  });

  test("rejects a truncated timestamp", () => {
    expect(() => readLittleEndianTimestamp(new Uint8Array(7), 0)).toThrow(RangeError);
  });
});
