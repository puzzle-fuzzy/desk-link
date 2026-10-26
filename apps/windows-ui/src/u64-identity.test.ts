import { describe, expect, test } from "bun:test";
import { isU64Identity } from "./u64-identity";

describe("Windows u64身份使用无损规范十进制字符串", () => {
  test.each([
    "1", "41", "9007199254740991", "9007199254740992", "9007199254740993",
    "18446744073709551614", "18446744073709551615",
  ])("保留合法身份 %s", (value) => {
    expect(isU64Identity(value)).toBe(true);
    expect(BigInt(value).toString()).toBe(value);
  });

  test.each([
    1, 41, 9007199254740991, 9007199254740992, 18446744073709551615,
    1n, 18446744073709551615n, 0, -1, Number.NaN, Number.POSITIVE_INFINITY,
    null, undefined, true, {}, [], new String("1"),
    "", "0", "00", "01", "00041", "-1", "+1", "1.0", "0.1", "1e2", "0x10",
    " 1", "1 ", "\t1", "1\n", "1\r\n", "1\u00a0", "1\0", "１", "١", "1_000",
    "18446744073709551616", "99999999999999999999", "184467440737095516150",
  ].map((value) => ({ value })))("无效身份 %#不被round、clamp或规范化", ({ value }) => {
    expect(isU64Identity(value)).toBe(false);
  });

  test("相邻且超过2^53的身份不会折叠成同一个number", () => {
    const before = "9007199254740992";
    const after = "9007199254740993";
    expect(Number(before)).toBe(Number(after));
    expect(isU64Identity(before)).toBe(true);
    expect(isU64Identity(after)).toBe(true);
    expect(before).not.toBe(after);
  });
});
