import { describe, expect, test } from "bun:test";

import {
  annexBNalTypes,
  hasAnnexBNalType,
  isH264Keyframe,
  prepareH264AccessUnit,
} from "./h264-annex-b";

describe("H.264 Annex B access units", () => {
  test("reads NAL types behind three-byte and four-byte start codes", () => {
    const bytes = new Uint8Array([
      0, 0, 0, 1, 0x09, 0x10,
      0, 0, 1, 0x67, 0x4d, 0x40, 0x28,
      0, 0, 0, 1, 0x68, 1,
      0, 0, 1, 0x65, 2,
    ]);
    expect(annexBNalTypes(bytes)).toEqual([9, 7, 8, 5]);
  });

  test("recognizes an IDR even when the transport flag is missing", () => {
    expect(isH264Keyframe(new Uint8Array([0, 0, 1, 0x65, 0x88]), false)).toBe(true);
    expect(isH264Keyframe(new Uint8Array([0, 0, 1, 0x41, 0x88]), false)).toBe(false);
    expect(isH264Keyframe(new Uint8Array([0, 0, 1, 0x41, 0x88]), true)).toBe(true);
  });

  test("scans for one NAL type without materializing the complete type list", () => {
    const bytes = new Uint8Array([
      0, 0, 1, 0x41, 0x88,
      0, 0, 0, 1, 0x65, 0x99,
    ]);
    expect(hasAnnexBNalType(bytes, 5)).toBe(true);
    expect(hasAnnexBNalType(bytes, 7)).toBe(false);
  });

  test("does not duplicate parameter sets already present in a keyframe", () => {
    const header = new Uint8Array([0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2]);
    const complete = new Uint8Array([
      0, 0, 1, 0x09, 0x10,
      0, 0, 1, 0x67, 1,
      0, 0, 1, 0x68, 2,
      0, 0, 1, 0x65, 3,
    ]);
    expect(prepareH264AccessUnit(header, complete, true)).toBe(complete);
  });

  test("prepends decoder parameters only when a keyframe lacks them", () => {
    const header = new Uint8Array([0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2]);
    const idr = new Uint8Array([0, 0, 1, 0x65, 3]);
    expect(Array.from(prepareH264AccessUnit(header, idr, true))).toEqual([
      ...header,
      ...idr,
    ]);
    expect(prepareH264AccessUnit(header, idr, false)).toBe(idr);
  });
});
