import { describe, expect, test } from "bun:test";

import { hashPassword, validatePassword, verifyPassword } from "./password";

describe("password policy", () => {
  test("uses Argon2id and verifies only the original password", async () => {
    const hash = await hashPassword("correct horse battery staple");
    expect(hash.startsWith("$argon2id$")).toBe(true);
    expect(await verifyPassword("correct horse battery staple", hash)).toBe(true);
    expect(await verifyPassword("wrong password", hash)).toBe(false);
  });

  test("rejects passwords outside the bounded policy", () => {
    expect(() => validatePassword("short")).toThrow();
    expect(() => validatePassword("x".repeat(129))).toThrow();
    expect(() => validatePassword("x".repeat(12))).not.toThrow();
  });
});
