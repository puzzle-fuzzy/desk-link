import { describe, expect, test } from "bun:test";

import { AccountStore } from "./store";

describe("AccountStore", () => {
  test("creates users, devices and sessions without sharing secrets across records", () => {
    const store = new AccountStore();
    const user = store.createUser({ email: "a@example.com", passwordHash: "argon2id$test", now: 100 });
    const device = store.upsertDevice({
      id: "device-a",
      userId: user.id,
      platform: "ios",
      name: "iPhone",
      now: 101,
    });
    const session = store.createSession({
      userId: user.id,
      deviceId: device.id,
      familyId: "family-a",
      accessTokenHash: "access-hash",
      accessExpiresAt: 200,
      refreshTokenHash: "refresh-hash",
      now: 101,
      expiresAt: 2_000,
    });

    expect(store.findUserByEmail("a@example.com")?.id).toBe(user.id);
    expect(store.findSessionByAccessHash("access-hash", 150)?.id).toBe(session.id);
    expect(store.findSessionByAccessHash("access-hash", 201)).toBeUndefined();
    expect(store.listDevices(user.id)).toHaveLength(1);
    store.close();
  });

  test("consumes action tokens exactly once", () => {
    const store = new AccountStore();
    const user = store.createUser({ email: "b@example.com", passwordHash: "hash", now: 100 });
    store.createActionToken({
      userId: user.id,
      purpose: "verify_email",
      tokenHash: "token-hash",
      now: 100,
      expiresAt: 200,
    });

    expect(store.consumeActionToken("token-hash", "verify_email", 150)?.userId).toBe(user.id);
    expect(store.consumeActionToken("token-hash", "verify_email", 151)).toBeUndefined();
    expect(store.consumeActionToken("token-hash", "reset_password", 151)).toBeUndefined();
    store.close();
  });
});
