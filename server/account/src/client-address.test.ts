import { expect, test } from "bun:test";
import { createAccountApp } from "./app";
import { clientAddress } from "./client-address";
import { loadAccountConfig } from "./config";
import { RateLimiter } from "./domain/rate-limit";

function forgot(index: number, ip: string) {
  return new Request("http://test/v1/account/password/forgot", { method: "POST", headers: {
    "content-type": "application/json", "x-forwarded-for": `198.51.100.${index}, 127.0.0.1`,
    "cf-connecting-ip": `198.51.100.${index}`, "x-real-ip": ip,
  }, body: JSON.stringify({ email: `synthetic-${index}@example.com` }) });
}

test("direct socket identity ignores forged forwarding headers and missing socket context shares one bucket", async () => {
  for (const context of [{ remoteAddress: "192.0.2.1" }, undefined]) {
    const app = createAccountApp();
    try {
      for (let index = 0; index < 5; index++) expect((await app.fetch(forgot(index, `198.51.100.${index}`), context)).status).toBe(202);
      expect((await app.fetch(forgot(10, "203.0.113.1"), context)).status).toBe(429);
      if (context) expect((await app.fetch(forgot(11, "203.0.113.1"), { remoteAddress: "192.0.2.2" })).status).toBe(202);
    } finally { await app.close(); }
  }
});

test("only an exact trusted immediate peer may supply a single valid overwritten X-Real-IP", async () => {
  const config = loadAccountConfig({ NODE_ENV: "test", DESKLINK_ACCOUNT_TRUSTED_PROXIES: "127.0.0.1,::1" });
  const app = createAccountApp({ config });
  try {
    for (let index = 0; index < 5; index++) expect((await app.fetch(forgot(index, "203.0.113.1"), { remoteAddress: "::ffff:127.0.0.1" })).status).toBe(202);
    expect((await app.fetch(forgot(10, "203.0.113.1"), { remoteAddress: "127.0.0.1" })).status).toBe(429);
    expect((await app.fetch(forgot(11, "203.0.113.2"), { remoteAddress: "127.0.0.1" })).status).toBe(202);
    expect(clientAddress(forgot(0, "192.0.2.1, 192.0.2.2"), "127.0.0.1", config.trustedProxyAddresses)).toBe("127.0.0.1");
    expect(clientAddress(forgot(0, "192.0.2.1"), "192.0.2.9", config.trustedProxyAddresses)).toBe("192.0.2.9");
  } finally { await app.close(); }
  expect(() => loadAccountConfig({ DESKLINK_ACCOUNT_TRUSTED_PROXIES: "*" })).toThrow("exact IP");
});

test("identity flooding cannot grow limiter state or evict an exhausted live quota", () => {
  const limiter = new RateLimiter(32);
  expect(limiter.allow("protected", 1, 3_600, 100)).toBe(true);
  for (let index = 0; index < 10_000; index++) limiter.allow(`synthetic-${index}`, 1, 60, 100);
  expect(limiter.size).toBe(32);
  expect(limiter.allow("protected", 1, 3_600, 101)).toBe(false);
  expect(limiter.allow("new", 1, 60, 101)).toBe(false);
  expect(limiter.allow("after-expiry", 1, 60, 3_700)).toBe(true);
  expect(limiter.size).toBeLessThanOrEqual(32);
});
