import { expect, test } from "bun:test";
import { canonicalU64, jsonExactUnsigned, parseU64 } from "./diagnostic-u64";

test("canonical decimal u64 spans the full range and preserves exact identity beyond safe integers", () => {
  for (const value of [0, 1, 7, Number.MAX_SAFE_INTEGER]) {
    expect(canonicalU64(value)).toBe(String(value));
    expect(canonicalU64(String(value))).toBe(String(value));
  }
  for (const value of ["9007199254740992", "9007199254740993", "18446744073709551614", "18446744073709551615"]) {
    expect(canonicalU64(value)).toBe(value);
    expect(parseU64(value)).toBe(BigInt(value));
  }
  expect(canonicalU64(7)).toBe(canonicalU64("7"));
  expect(new Set([canonicalU64("9007199254740992"), canonicalU64("9007199254740993")]).size).toBe(2);
  expect(parseU64("18446744073709551614")! < parseU64("18446744073709551615")!).toBe(true);
});

test("exact aggregate JSON keeps safe legacy numbers and serializes larger sums without BigInt", () => {
  expect(jsonExactUnsigned(0n)).toBe(0);
  expect(jsonExactUnsigned(9_007_199_254_740_991n)).toBe(Number.MAX_SAFE_INTEGER);
  expect(jsonExactUnsigned(9_007_199_254_740_992n)).toBe("9007199254740992");
  const sum = 18_446_744_073_709_551_615n * 2n;
  expect(jsonExactUnsigned(sum)).toBe("36893488147419103230");
  expect(JSON.stringify({ counter: jsonExactUnsigned(sum) })).toBe('{"counter":"36893488147419103230"}');
  expect(() => jsonExactUnsigned(-1n)).toThrow(RangeError);
});

test("invalid decimal forms and out-of-range or already-rounded numbers are rejected without coercion", () => {
  const invalid = [
    "", "00", "01", "00000000000000000001", "+1", "-1", "-0", "1.0", "1.5", "1e3", "1E3", "0x10", "1_000",
    " 1", "1 ", "1\n", "\t1", "١", "１", "Infinity", "NaN",
    "18446744073709551616", "99999999999999999999", "100000000000000000000", "9".repeat(100_000),
    Number.MAX_SAFE_INTEGER + 1, -1, -0, .5, Infinity, NaN, true, false, null, undefined, 1n,
    { toString() { throw new Error("must never coerce an untrusted object"); } },
  ];
  for (const value of invalid) {
    expect(canonicalU64(value)).toBeNull();
    expect(parseU64(value)).toBeNull();
  }
});
