/**
 * Reads a little-endian u64 timestamp without allocating a DataView object.
 * Capture timestamps are current Unix microseconds and therefore remain
 * exactly representable as a JavaScript number for the supported lifetime.
 */
export function readLittleEndianTimestamp(bytes: Uint8Array, offset: number): number {
  if (offset < 0 || offset + 8 > bytes.length) {
    throw new RangeError("视频时间戳超出数据范围");
  }
  const low = (
    bytes[offset]!
    | (bytes[offset + 1]! << 8)
    | (bytes[offset + 2]! << 16)
    | (bytes[offset + 3]! << 24)
  ) >>> 0;
  const high = (
    bytes[offset + 4]!
    | (bytes[offset + 5]! << 8)
    | (bytes[offset + 6]! << 16)
    | (bytes[offset + 7]! << 24)
  ) >>> 0;
  return high * 0x1_0000_0000 + low;
}
