import { describe, expect, test } from "bun:test";

import { h264CodecFromSequenceHeader, videoConfigKey } from "./video-config";

describe("H.264 decoder configuration", () => {
  test("reads the SPS profile from three-byte and four-byte Annex B headers", () => {
    expect(h264CodecFromSequenceHeader(new Uint8Array([0, 0, 1, 0x67, 0x64, 0, 0x29]))).toBe(
      "avc1.640029",
    );
    expect(h264CodecFromSequenceHeader(new Uint8Array([0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f]))).toBe(
      "avc1.42E01F",
    );
  });

  test("uses a conservative baseline value for malformed or missing SPS data", () => {
    expect(h264CodecFromSequenceHeader(new Uint8Array())).toBe("avc1.42E01E");
    expect(h264CodecFromSequenceHeader(new Uint8Array([0, 0, 1, 0x68, 1, 2, 3]))).toBe(
      "avc1.42E01E",
    );
  });

  test("distinguishes stream restarts from configuration revisions", () => {
    expect(videoConfigKey({ streamId: 8, configVersion: 2 })).toBe("8:2");
    expect(videoConfigKey({ streamId: 9, configVersion: 2 })).not.toBe("8:2");
  });
});
