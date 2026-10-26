import { expect, test } from "bun:test";
import { createAccountApp } from "./app";
import { loadAccountConfig } from "./config";
import { AccountStore } from "./db/store";
import type { ActionTokenPurpose } from "./db/types";
import { RateLimiter } from "./domain/rate-limit";
import { createOpaqueToken } from "./domain/tokens";

const now = 1_000_000;
class ObservedLimiter extends RateLimiter {
  readonly keys: string[] = [];
  override allow(key: string, limit: number, windowSeconds: number, current: number) {
    this.keys.push(key);
    return super.allow(key, limit, windowSeconds, current);
  }
}
function fixture(rateLimiter?: RateLimiter) {
  const store = new AccountStore();
  let sequence = 0;
  const app = createAccountApp({ store, config: loadAccountConfig({ NODE_ENV: "test" }), now: () => now, rateLimiter,
    passwords: { async hash() { return "synthetic-reset-hash"; }, async verify() { return true; } } });
  return { app, store, issue(purpose: ActionTokenPurpose, index: number) {
    const user = store.createUser({ email: `synthetic-action-${index}-${sequence++}@example.com`, passwordHash: "synthetic-initial-hash", now });
    const token = createOpaqueToken();
    const record = store.createActionToken({ userId: user.id, purpose, tokenHash: token.hash, now, expiresAt: now + 3_600 });
    return { value: token.value, id: record.id, userId: user.id };
  } };
}
function verifyGet(token: string) {
  return new Request(`http://synthetic/v1/account/verify-email?token=${encodeURIComponent(token)}`);
}
function tokenRequest(purpose: ActionTokenPurpose, token: string, extra: Record<string, unknown> = {}) {
  const path = purpose === "verify_email" ? "verify-email" : "password/reset";
  return new Request(`http://synthetic/v1/account/${path}`, { method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ token, password: "synthetic-new-password", ...extra }) });
}

for (const purpose of ["reset_password", "verify_email"] as const) {
  test(`eleven independent ${purpose} tokens do not share an email:missing bucket`, async () => {
    const { app, store, issue } = fixture();
    try {
      const expected = purpose === "verify_email" ? 200 : 204;
      const tokens = Array.from({ length: 11 }, (_, index) => issue(purpose, index));
      const statuses: number[] = [];
      for (const [index, token] of tokens.entries()) {
        statuses.push((await app.fetch(tokenRequest(purpose, token.value), { remoteAddress: `192.0.2.${index + 1}` })).status);
      }
      expect(statuses).toEqual(Array(11).fill(expected));
      for (const token of tokens) expect(store.findActionTokenById(token.id)?.consumedAt).toBe(now);
    } finally { await app.close(); }
  });

  test(`${purpose} still enforces its original socket IP quota despite forged forwarding headers`, async () => {
    const { app, store, issue } = fixture();
    try {
      for (let index = 0; index < 10; index++) {
        const token = issue(purpose, index);
        const request = tokenRequest(purpose, token.value);
        request.headers.set("x-forwarded-for", `198.51.100.${index + 1}`);
        request.headers.set("x-real-ip", `198.51.100.${index + 1}`);
        expect((await app.fetch(request, { remoteAddress: "192.0.2.1" })).status).toBe(purpose === "verify_email" ? 200 : 204);
      }
      const token = issue(purpose, 11);
      expect((await app.fetch(tokenRequest(purpose, token.value), { remoteAddress: "192.0.2.1" })).status).toBe(429);
      expect(store.findActionTokenById(token.id)?.consumedAt).toBeNull();
      expect((await app.fetch(tokenRequest(purpose, token.value), { remoteAddress: "192.0.2.2" })).status).toBe(purpose === "verify_email" ? 200 : 204);
      expect(store.findActionTokenById(token.id)?.consumedAt).toBe(now);
    } finally { await app.close(); }
  });
}

test("GET and POST verification share one socket IP quota", async () => {
  const { app, store, issue } = fixture();
  try {
    for (let index = 0; index < 10; index++) {
      const token = issue("verify_email", index);
      const request = index % 2 === 0 ? verifyGet(token.value) : tokenRequest("verify_email", token.value);
      expect((await app.fetch(request, { remoteAddress: "192.0.2.1" })).status).toBe(200);
    }
    const token = issue("verify_email", 11);
    expect((await app.fetch(verifyGet(token.value), { remoteAddress: "192.0.2.1" })).status).toBe(429);
    expect((await app.fetch(tokenRequest("verify_email", token.value), { remoteAddress: "192.0.2.1" })).status).toBe(429);
    expect(store.findActionTokenById(token.id)?.consumedAt).toBeNull();
    expect((await app.fetch(verifyGet(token.value), { remoteAddress: "192.0.2.2" })).status).toBe(200);
  } finally { await app.close(); }
});

test("POST verification has a shared global 120/min budget before reading the body", async () => {
  const { app, store, issue } = fixture();
  try {
    for (let index = 0; index < 120; index++) {
      const token = issue("verify_email", index);
      expect((await app.fetch(tokenRequest("verify_email", token.value), { remoteAddress: `192.0.2.${index + 1}` })).status).toBe(200);
    }
    for (const purpose of ["verify_email", "reset_password"] as const) {
      const token = issue(purpose, 121);
      const request = tokenRequest(purpose, token.value);
      expect((await app.fetch(request, { remoteAddress: "198.51.100.1" })).status).toBe(429);
      expect(request.bodyUsed).toBe(false);
      expect(store.findActionTokenById(token.id)?.consumedAt).toBeNull();
    }
  } finally { await app.close(); }
});

test("GET verification and existing POST actions share the same global request budget", async () => {
  const { app, store, issue } = fixture();
  try {
    for (let index = 0; index < 120; index++) {
      const purpose = index % 2 === 0 ? "verify_email" : "reset_password";
      const token = issue(purpose, index);
      const request = purpose === "verify_email" ? verifyGet(token.value) : tokenRequest(purpose, token.value);
      expect((await app.fetch(request, { remoteAddress: `192.0.2.${index + 1}` })).status).toBe(purpose === "verify_email" ? 200 : 204);
    }
    const token = issue("verify_email", 121);
    expect((await app.fetch(verifyGet(token.value), { remoteAddress: "198.51.100.1" })).status).toBe(429);
    expect(store.findActionTokenById(token.id)?.consumedAt).toBeNull();
  } finally { await app.close(); }
});

test("token-body email injection creates no email bucket and leaves real-email action quotas intact", async () => {
  const limiter = new ObservedLimiter();
  const { app, store, issue } = fixture(limiter);
  const email = "target@example.com";
  try {
    for (const purpose of ["reset_password", "verify_email"] as const) {
      for (let index = 0; index < 11; index++) {
        const token = issue(purpose, index);
        const injected = index % 2 === 0 ? email : { invalid: true };
        const response = await app.fetch(tokenRequest(purpose, token.value, { email: injected }), { remoteAddress: `192.0.2.${index + 1}` });
        expect(response.status).toBe(purpose === "verify_email" ? 200 : 204);
        expect(store.findActionTokenById(token.id)?.consumedAt).toBe(now);
      }
    }
    expect(limiter.keys.some(key => key.includes(":email:"))).toBe(false);
    const responses = await Promise.all(Array.from({ length: 5 }, (_, index) => app.fetch(new Request("http://synthetic/v1/account/password/forgot", {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ email: "  TARGET@example.com " }),
    }), { remoteAddress: `198.51.100.${index + 1}` })));
    expect(responses.map(response => response.status)).toEqual(Array(5).fill(202));
    const blocked = await app.fetch(new Request("http://synthetic/v1/account/password/forgot", {
      method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ email }),
    }), { remoteAddress: "198.51.100.6" });
    expect(blocked.status).toBe(429);
    expect(limiter.keys.filter(key => key.includes(":email:"))).toEqual(Array(6).fill(`forgot-password:email:${email}`));
  } finally { await app.close(); }
});

test("missing and invalid email input uses only the unchanged IP quota", async () => {
  const limiter = new ObservedLimiter();
  const { app } = fixture(limiter);
  try {
    for (const [path, limit] of [["register", 5], ["login", 12], ["password/forgot", 5], ["verify-email/resend", 5]] as const) {
      for (const [index, body] of [{}, { email: "not-an-email" }, { email: "x".repeat(321) }].entries()) {
        expect((await app.fetch(new Request(`http://synthetic/v1/account/${path}`, {
          method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body),
        }), { remoteAddress: `192.0.2.${index + 1}` })).status).toBe(400);
      }
      const responses = await Promise.all(Array.from({ length: limit + 1 }, () => app.fetch(new Request(`http://synthetic/v1/account/${path}`, {
        method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ email: "not-an-email" }),
      }), { remoteAddress: "198.51.100.1" })));
      expect(responses.filter(response => response.status === 400)).toHaveLength(limit);
      expect(responses.filter(response => response.status === 429)).toHaveLength(1);
    }
    expect(limiter.keys.some(key => key.includes(":email:"))).toBe(false);
  } finally { await app.close(); }
});
