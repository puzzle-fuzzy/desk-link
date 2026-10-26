import { expect, spyOn, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createAccountApp } from "./app";
import { AccountStore } from "./db/store";
import { DUMMY_PASSWORD_HASH, hashPassword, PasswordWorkBudget, verifyPassword } from "./domain/password";
import { createOpaqueToken } from "./domain/tokens";

function fixture(store = new AccountStore(), now = 1_000) {
  const user = store.createUser({ email: "synthetic-reset@example.com", passwordHash: DUMMY_PASSWORD_HASH, now });
  const device = store.upsertDevice({ userId: user.id, id: "synthetic-reset-device", platform: "windows", name: "Synthetic", now });
  const session = store.createSession({ userId: user.id, deviceId: device.id, familyId: "synthetic-reset-family",
    accessTokenHash: "synthetic-reset-access-hash", accessExpiresAt: 1_500, refreshTokenHash: "synthetic-reset-refresh-hash", now, expiresAt: 5_000 });
  const token = createOpaqueToken();
  const record = store.createActionToken({ userId: user.id, purpose: "reset_password", tokenHash: token.hash, now, expiresAt: now + 1_000 });
  return { store, user, session, token, record };
}
function reset(token: string, password = "synthetic reset password") {
  return new Request("http://test/v1/account/password/reset", { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ token, password }) });
}
function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(done => { resolve = done; });
  return { promise, resolve };
}
function resetAudits(store: AccountStore) {
  return (store.database.query("SELECT count(*) AS n FROM audit_events WHERE action = 'account.password_reset'").get() as { n: number }).n;
}
function unchanged(value: ReturnType<typeof fixture>) {
  expect(value.store.findUserById(value.user.id)?.passwordHash).toBe(DUMMY_PASSWORD_HASH);
  expect(value.store.findActionTokenById(value.record.id)?.consumedAt).toBeNull();
  expect(value.store.findSessionById(value.session.id)?.revokedAt).toBeNull();
  expect(resetAudits(value.store)).toBe(0);
}

test("saturated real password budget returns 429, then the same reset token succeeds after draining", async () => {
  const { store, user, session, token, record } = fixture();
  const budget = new PasswordWorkBudget();
  const gate = deferred();
  // Exercise the real default admission boundary: 2 running + 16 queued.
  const occupied = Array.from({ length: 18 }, () => budget.run(() => gate.promise));
  const app = createAccountApp({ store, now: () => 1_000, passwords: {
    hash: password => budget.run(() => hashPassword(password)), verify: verifyPassword,
  } });
  try {
    expect((await app.fetch(reset(token.value))).status).toBe(429);
    unchanged({ store, user, session, token, record });
    gate.resolve();
    await Promise.all(occupied);
    expect((await app.fetch(reset(token.value))).status).toBe(204);
    expect(store.findActionTokenById(record.id)?.consumedAt).toBe(1_000);
    expect(await verifyPassword("synthetic reset password", store.findUserById(user.id)!.passwordHash)).toBe(true);
    expect(store.findSessionById(session.id)?.revokedAt).toBe(1_000);
    expect(resetAudits(store)).toBe(1);
    expect((await app.fetch(reset(token.value))).status).toBe(400);
  } finally {
    gate.resolve(); await Promise.all(occupied); await app.close();
  }
});

test("two file connections that finish hashing the same token commit exactly one reset", async () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-reset-race-"));
  const path = join(directory, "local.sqlite");
  const value = fixture(new AccountStore(path));
  const second = new AccountStore(path);
  const gate = deferred(), entered = deferred();
  let hashing = 0;
  const operations = {
    async hash(password: string) { if (++hashing === 2) entered.resolve(); await gate.promise; return hashPassword(password); },
    verify: verifyPassword,
  };
  const apps = [createAccountApp({ store: value.store, passwords: operations, now: () => 1_000 }),
    createAccountApp({ store: second, passwords: operations, now: () => 1_000 })];
  const passwords = ["synthetic first reset password", "synthetic second reset password"];
  const requests = apps.map((app, index) => app.fetch(reset(value.token.value, passwords[index])));
  try {
    await entered.promise;
    unchanged(value);
    gate.resolve();
    const responses = await Promise.all(requests);
    expect(responses.map(response => response.status).sort()).toEqual([204, 400]);
    const winner = responses.findIndex(response => response.status === 204);
    const passwordHash = value.store.findUserById(value.user.id)!.passwordHash;
    expect(await verifyPassword(passwords[winner], passwordHash)).toBe(true);
    expect(await verifyPassword(passwords[1 - winner], passwordHash)).toBe(false);
    expect(resetAudits(value.store)).toBe(1);
    expect(value.store.findActionTokenById(value.record.id)?.consumedAt).toBe(1_000);
    expect(value.store.findSessionById(value.session.id)?.revokedAt).toBe(1_000);
  } finally {
    gate.resolve(); await Promise.allSettled(requests);
    for (const app of apps) await app.close();
    rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 });
  }
});

test("hash rejection preserves the token and all account state for a same-token retry", async () => {
  const value = fixture();
  let fails = true;
  const app = createAccountApp({ store: value.store, now: () => 1_000, passwords: {
    async hash(password) { if (fails) throw new Error("synthetic hash failure"); return hashPassword(password); }, verify: verifyPassword,
  } });
  const log = spyOn(console, "error").mockImplementation(() => {});
  try {
    expect((await app.fetch(reset(value.token.value))).status).toBe(500);
    unchanged(value);
    fails = false;
    expect((await app.fetch(reset(value.token.value))).status).toBe(204);
    expect(resetAudits(value.store)).toBe(1);
  } finally { log.mockRestore(); await app.close(); }
});

test.each(["password", "audit"])("a %s database write failure rolls consumption and account effects back together", async stage => {
  const value = fixture();
  const app = createAccountApp({ store: value.store, now: () => 1_000 });
  const trigger = stage === "password"
    ? "BEFORE UPDATE OF password_hash ON users"
    : "BEFORE INSERT ON audit_events WHEN NEW.action = 'account.password_reset'";
  value.store.database.exec(`CREATE TRIGGER synthetic_reset_failure ${trigger} BEGIN SELECT RAISE(ABORT, 'synthetic reset failure'); END`);
  const log = spyOn(console, "error").mockImplementation(() => {});
  try {
    expect((await app.fetch(reset(value.token.value))).status).toBe(500);
    unchanged(value);
    expect(value.store.database.inTransaction).toBe(false);
    value.store.database.exec("DROP TRIGGER synthetic_reset_failure");
    expect((await app.fetch(reset(value.token.value))).status).toBe(204);
    expect(resetAudits(value.store)).toBe(1);
  } finally { log.mockRestore(); await app.close(); }
});

test("expiry at the exact boundary while hashing is rechecked before consuming", async () => {
  const value = fixture();
  const gate = deferred(), entered = deferred();
  let now = 1_000;
  const app = createAccountApp({ store: value.store, now: () => now, passwords: {
    async hash() { entered.resolve(); await gate.promise; return DUMMY_PASSWORD_HASH; }, verify: verifyPassword,
  } });
  const pending = app.fetch(reset(value.token.value));
  try {
    await entered.promise;
    now = value.record.expiresAt;
    gate.resolve();
    expect((await pending).status).toBe(400);
    unchanged(value);
  } finally { gate.resolve(); await pending; await app.close(); }
});

test("revocation while hashing cannot be overwritten by a stale successful hash", async () => {
  const value = fixture();
  const gate = deferred(), entered = deferred();
  const app = createAccountApp({ store: value.store, now: () => 1_000, passwords: {
    async hash() { entered.resolve(); await gate.promise; return DUMMY_PASSWORD_HASH; }, verify: verifyPassword,
  } });
  const pending = app.fetch(reset(value.token.value));
  try {
    await entered.promise;
    value.store.invalidateActionToken(value.record.id, 999);
    gate.resolve();
    expect((await pending).status).toBe(400);
    expect(value.store.findActionTokenById(value.record.id)?.consumedAt).toBe(999);
    expect(value.store.findUserById(value.user.id)?.passwordHash).toBe(DUMMY_PASSWORD_HASH);
    expect(value.store.findSessionById(value.session.id)?.revokedAt).toBeNull();
    expect(resetAudits(value.store)).toBe(0);
  } finally { gate.resolve(); await pending; await app.close(); }
});

test("hash waiting holds no writer lock, and lock contention after hashing preserves same-token retry", async () => {
  const directory = mkdtempSync(join(tmpdir(), "desklink-reset-lock-"));
  const path = join(directory, "local.sqlite");
  const value = fixture(new AccountStore(path));
  const competitor = new AccountStore(path);
  const gate = deferred(), entered = deferred();
  const app = createAccountApp({ store: value.store, now: () => 1_000, passwords: {
    async hash(password) { entered.resolve(); await gate.promise; return hashPassword(password); }, verify: verifyPassword,
  } });
  const log = spyOn(console, "error").mockImplementation(() => {});
  const pending = app.fetch(reset(value.token.value));
  let locked = false;
  try {
    await entered.promise;
    expect(value.store.database.inTransaction).toBe(false);
    competitor.database.exec("BEGIN IMMEDIATE"); locked = true;
    gate.resolve();
    expect((await pending).status).toBe(500);
    unchanged(value);
    competitor.database.exec("ROLLBACK"); locked = false;
    expect((await app.fetch(reset(value.token.value))).status).toBe(204);
  } finally {
    gate.resolve(); await pending;
    if (locked) competitor.database.exec("ROLLBACK");
    log.mockRestore(); await app.close(); competitor.close();
    rmSync(directory, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 });
  }
});

test("missing, expired, consumed and wrong-purpose bearer tokens share the same error and spend no hash budget", async () => {
  const value = fixture();
  const expired = createOpaqueToken(), consumed = createOpaqueToken(), verification = createOpaqueToken();
  value.store.createActionToken({ userId: value.user.id, purpose: "reset_password", tokenHash: expired.hash, now: 1, expiresAt: 1_000 });
  const used = value.store.createActionToken({ userId: value.user.id, purpose: "reset_password", tokenHash: consumed.hash, now: 1, expiresAt: 2_000 });
  value.store.invalidateActionToken(used.id, 2);
  value.store.createActionToken({ userId: value.user.id, purpose: "verify_email", tokenHash: verification.hash, now: 1, expiresAt: 2_000 });
  let hashes = 0;
  const app = createAccountApp({ store: value.store, now: () => 1_000, passwords: {
    async hash() { hashes++; return DUMMY_PASSWORD_HASH; }, verify: verifyPassword,
  } });
  try {
    const errors: unknown[] = [];
    for (const token of [createOpaqueToken().value, expired.value, consumed.value, verification.value]) {
      const response = await app.fetch(reset(token));
      expect(response.status).toBe(400);
      errors.push(await response.json());
    }
    for (const error of errors) expect(error).toEqual(errors[0]);
    expect(hashes).toBe(0);
    unchanged(value);
  } finally { await app.close(); }
});
