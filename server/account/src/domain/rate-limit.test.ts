import { describe, expect, test } from "bun:test";

import { RateLimiter } from "./rate-limit";

describe("RateLimiter", () => {
  test("allows requests again only when their own window expires", () => {
    const limiter = new RateLimiter();
    expect(limiter.allow("login:ip:local", 2, 60, 100)).toBe(true);
    expect(limiter.allow("login:ip:local", 2, 60, 101)).toBe(true);
    expect(limiter.allow("login:ip:local", 2, 60, 159)).toBe(false);
    expect(limiter.allow("login:ip:local", 2, 60, 160)).toBe(true);
  });

  test("short-window cleanup preserves an exhausted hourly limit", () => {
    const limiter = new RateLimiter();
    const registrationKey = "register:email:test@example.com";
    for (let request = 0; request < 5; request += 1) {
      expect(limiter.allow(registrationKey, 5, 3_600, 100)).toBe(true);
    }
    expect(limiter.allow(registrationKey, 5, 3_600, 161)).toBe(false);

    for (let client = 0; client < 255; client += 1) {
      limiter.allow(`login:ip:${client}`, 12, 60, 161);
    }

    expect(limiter.allow(registrationKey, 5, 3_600, 161)).toBe(false);
    expect(limiter.allow(registrationKey, 5, 3_600, 3_699)).toBe(false);
    expect(limiter.allow(registrationKey, 5, 3_600, 3_700)).toBe(true);
  });
});
