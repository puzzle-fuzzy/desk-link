import { describe, expect, test } from "bun:test";

import { createOpaqueToken, equalOpaqueTokenHashes, hashOpaqueToken } from "./tokens";

describe("opaque tokens", () => {
  test("returns a random bearer value and only its hash for storage", () => {
    const token = createOpaqueToken();
    expect(token.value.length).toBeGreaterThan(20);
    expect(token.hash).toBe(hashOpaqueToken(token.value));
    expect(token.hash).not.toContain(token.value);
  });

  test("compares hashes without accepting different lengths", () => {
    const hash = hashOpaqueToken("token");
    expect(equalOpaqueTokenHashes(hash, hash)).toBe(true);
    expect(equalOpaqueTokenHashes(hash, hash.slice(1))).toBe(false);
  });
});
